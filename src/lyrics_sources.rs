//! Formatos de las fuentes de letras con tiempos por palabra o sílaba (la red, en `api.rs`):
//!
//! - BiniLyrics (lrc.red): una búsqueda (`results`, cada uno con `lyricsUrl` y `timing_type`
//!   «word» o «line») y la letra en TTML, el formato de Apple: `<div>` por estrofa, `<p begin
//!   end>` por renglón y `<span begin end>` por palabra o sílaba.
//! - Lyrics+ (la API pública que usa YouLy+; de ahí, Musixmatch): JSON «KPoe», con `lyrics` de
//!   renglones (`time`, `duration`, `text`, `syllabus` con sus trozos y `element.songPartIndex`).
//!
//! Entre estrofas se mete un renglón vacío (la pausa del panel de la letra), que empieza cuando
//! acaba el renglón anterior: durante la parte sin voz, lo cantado queda en blanco.

use serde_json::Value;

use crate::model::{LyricLine, Lyrics, Syllable};

/// Segundos de un tiempo TTML («31.405», «1:05.655», «1:02:03.5», «12.3s») en milisegundos.
fn ttml_ms(t: &str) -> Option<u32> {
    let t = t.trim().trim_end_matches('s');
    let mut secs = 0.0f64;
    for part in t.split(':') {
        secs = secs * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    Some((secs * 1000.0).round() as u32)
}

/// Valor de un atributo (`name="…"`) de una etiqueta.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(i) = rest.find(name) {
        let before_ok = i == 0 || rest[..i].ends_with(char::is_whitespace);
        let after = &rest[i + name.len()..];
        if before_ok {
            if let Some(v) = after.strip_prefix("=\"") {
                return v.find('"').map(|j| &v[..j]);
            }
        }
        rest = after;
    }
    None
}

/// Entidades XML de un texto.
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(j) = tail.find(';').filter(|&j| j <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let ent = &tail[1..j];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[j + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Un renglón terminado: sin espacios a los lados (los del principio se descuentan de la primera
/// sílaba) y sin sílabas vacías.
fn finish_line(words: String, mut syllables: Vec<Syllable>, start_ms: u32, end_ms: Option<u32>) -> LyricLine {
    let lead = words.chars().count() - words.trim_start().chars().count();
    let mut skip = lead;
    for s in syllables.iter_mut() {
        let k = skip.min(s.chars);
        s.chars -= k;
        skip -= k;
    }
    syllables.retain(|s| s.chars > 0);
    LyricLine { start_ms, words: words.trim().to_string(), syllables, end_ms }
}

/// Mete un renglón vacío entre estrofas (`breaks[i]`: el renglón `i` empieza una estrofa nueva).
fn with_stanza_gaps(lines: Vec<LyricLine>, breaks: &[bool]) -> Vec<LyricLine> {
    let mut out: Vec<LyricLine> = Vec::with_capacity(lines.len() + 8);
    for (i, line) in lines.into_iter().enumerate() {
        if i > 0 && breaks.get(i).copied().unwrap_or(false) {
            let prev = out.last().map(|l| l.end_ms.unwrap_or(l.start_ms)).unwrap_or(0);
            out.push(LyricLine { start_ms: prev.min(line.start_ms), ..Default::default() });
        }
        out.push(line);
    }
    out
}

fn lyrics(track_id: &str, lines: Vec<LyricLine>, provider: &str) -> Option<Lyrics> {
    if lines.iter().filter(|l| !l.words.is_empty()).count() < 2 {
        return None;
    }
    let sync_type = if lines.iter().any(|l| !l.syllables.is_empty()) { "SYLLABLE_SYNCED" } else { "LINE_SYNCED" };
    Some(Lyrics { track_id: track_id.to_string(), sync_type: sync_type.into(), lines, provider: provider.into() })
}

/// Letra en TTML (Apple, lrc.red). Las voces de fondo (`ttm:role="x-bg"`) no se pintan: no
/// llevan el orden de la voz principal.
pub fn parse_ttml(track_id: &str, text: &str, provider: &str) -> Option<Lyrics> {
    let body = &text[text.find("<body")?..];
    let mut lines: Vec<LyricLine> = Vec::new();
    let mut breaks: Vec<bool> = Vec::new();
    let mut new_div = false;
    // Renglón abierto: comienzo, final, texto, sílabas; y profundidad de <span> y de la voz de
    // fondo (0: fuera).
    let mut cur: Option<(u32, Option<u32>, String, Vec<Syllable>)> = None;
    let (mut depth, mut bg) = (0usize, 0usize);
    let mut rest = body;
    while let Some(i) = rest.find('<') {
        let text = &rest[..i];
        if let (Some((_, _, words, syl)), 0) = (cur.as_mut(), bg) {
            let t = unescape(text);
            if let Some(last) = syl.last_mut() {
                last.chars += t.chars().count();
            }
            words.push_str(&t);
        }
        let Some(j) = rest[i..].find('>') else { break };
        let tag = &rest[i + 1..i + j];
        rest = &rest[i + j + 1..];
        let closing = tag.starts_with('/');
        let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        let self_closing = tag.ends_with('/');
        match (name, closing) {
            ("div", false) => new_div = true,
            ("p", false) => {
                let begin = attr(tag, "begin").and_then(ttml_ms).unwrap_or(0);
                let end = attr(tag, "end").and_then(ttml_ms);
                cur = Some((begin, end, String::new(), Vec::new()));
                depth = 0;
                bg = 0;
            }
            ("p", true) => {
                if let Some((begin, end, words, syl)) = cur.take() {
                    let line = finish_line(words, syl, begin, end);
                    if !line.words.is_empty() {
                        breaks.push(new_div && !lines.is_empty());
                        new_div = false;
                        lines.push(line);
                    }
                }
            }
            ("span", false) if !self_closing => {
                depth += 1;
                if bg == 0 && attr(tag, "ttm:role").is_some_and(|r| r.contains("bg")) {
                    bg = depth;
                } else if bg == 0 {
                    if let (Some((_, _, _, syl)), Some(t)) = (cur.as_mut(), attr(tag, "begin").and_then(ttml_ms)) {
                        syl.push(Syllable { start_ms: t, chars: 0 });
                    }
                }
            }
            ("span", true) => {
                if bg == depth {
                    bg = 0;
                }
                depth = depth.saturating_sub(1);
            }
            ("br", _) => {
                if let Some((_, _, words, _)) = cur.as_mut() {
                    words.push(' ');
                }
            }
            _ => {}
        }
    }
    lyrics(track_id, with_stanza_gaps(lines, &breaks), provider)
}

/// Letra en el JSON de Lyrics+ («KPoe»). Las sílabas de voces de fondo no se pintan.
pub fn parse_kpoe(track_id: &str, v: &Value, provider: &str) -> Option<Lyrics> {
    let arr = v.get("lyrics")?.as_array()?;
    let ms = |v: Option<&Value>| v.and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|s| s.parse().ok()))).map(|x| x.max(0.0) as u32);
    let mut lines = Vec::with_capacity(arr.len());
    let mut breaks = Vec::with_capacity(arr.len());
    let mut part: Option<i64> = None;
    for l in arr {
        let start = ms(l.get("time")).unwrap_or(0);
        let end = ms(l.get("duration")).filter(|&d| d > 0).map(|d| start + d);
        let mut words = String::new();
        let mut syl = Vec::new();
        for s in l.get("syllabus").and_then(|s| s.as_array()).into_iter().flatten() {
            if s.get("isBackground").and_then(|b| b.as_bool()).unwrap_or(false) {
                continue;
            }
            let t = s.get("text").and_then(|t| t.as_str()).unwrap_or("");
            if let Some(at) = ms(s.get("time")) {
                syl.push(Syllable { start_ms: at, chars: t.chars().count() });
            }
            words.push_str(t);
        }
        if syl.is_empty() {
            words = l.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
        }
        let line = finish_line(words, syl, start, end);
        if line.words.is_empty() {
            continue;
        }
        let this = l.get("element").and_then(|e| e.get("songPartIndex")).and_then(|p| p.as_i64());
        breaks.push(!lines.is_empty() && this.is_some() && this != part);
        part = this.or(part);
        lines.push(line);
    }
    lyrics(track_id, with_stanza_gaps(lines, &breaks), provider)
}

/// De las respuestas de las fuentes por orden de preferencia (`None`: aún no respondió;
/// `Some(None)`: no la tiene), la que gana ya: la primera con letra, cuando todas las de delante
/// ya dijeron que no. Que una peor responda antes no la hace ganar.
pub fn winner<T>(got: &[Option<Option<T>>]) -> Option<&T> {
    for slot in got {
        match slot {
            Some(Some(l)) => return Some(l),
            Some(None) => continue,
            None => return None,
        }
    }
    None
}

/// La mejor que ha llegado hasta ahora, aunque falte por responder alguna mejor (para ir
/// enseñándola), con su hueco.
pub fn best_so_far<T>(got: &[Option<Option<T>>]) -> Option<(usize, &T)> {
    got.iter().enumerate().find_map(|(k, s)| s.as_ref().and_then(|l| l.as_ref()).map(|l| (k, l)))
}

/// Minúsculas y solo letras y números (lo demás, espacios simples).
fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim().to_string()
}

/// El título para buscar: sin «(feat. …)», «[…]» ni «- Remastered» (tal cual lo demás).
pub fn search_title(s: &str) -> &str {
    let mut t = s;
    for cut in [" (", " [", " - "] {
        if let Some(i) = t.find(cut) {
            t = &t[..i];
        }
    }
    t.trim()
}

/// El título sin «(feat. …)», «- Remastered», etc.
fn base_title(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut t = lower.as_str();
    for cut in [" (", " [", " - ", " feat", " ft.", " with "] {
        if let Some(i) = t.find(cut) {
            t = &t[..i];
        }
    }
    norm(t)
}

/// Versiones que no son la original (si el título que suena no lo dice, restan).
const MODIFIERS: [&str; 16] =
    ["slowed", "reverb", "remix", "sped up", "speed up", "cover", "karaoke", "instrumental", "acoustic", "live", "nightcore", "mashup", "edit", "version", "versión", "mix"];

/// De los resultados de la búsqueda de BiniLyrics, el `lyricsUrl` de la versión que es esta
/// canción con tiempos del tipo pedido («word» o «line»). Puntuación como la de YouLy+: título,
/// artista, álbum y duración (más de 2 s de diferencia la descarta).
pub fn pick_lrcred(results: &[Value], title: &str, artist: &str, album: &str, duration_s: u32, timing: &str) -> Option<String> {
    let (full, base, art, alb) = (norm(title), base_title(title), norm(artist), norm(album));
    let first_artist = norm(artist.split(',').next().unwrap_or(artist));
    let lower = format!("{} {}", title.to_lowercase(), album.to_lowercase());
    let mine: Vec<&str> = MODIFIERS.iter().copied().filter(|m| lower.contains(m)).collect();
    let mut best: Option<(i32, String)> = None;
    for r in results {
        if r.get("timing_type").and_then(|t| t.as_str()) != Some(timing) {
            continue;
        }
        let Some(url) = r.get("lyricsUrl").and_then(|u| u.as_str()) else { continue };
        let s = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("");
        let dur = r.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0);
        if duration_s > 0 && dur > 0.0 && (dur - duration_s as f64).abs() > 2.0 {
            continue;
        }
        let (r_full, r_base, r_art, r_alb) = (norm(s("track_name")), base_title(s("track_name")), norm(s("artist_name")), norm(s("album_name")));
        let mut score = 0;
        score += if !full.is_empty() && full == r_full {
            50
        } else if !base.is_empty() && base == r_base {
            35
        } else if !base.is_empty() && !r_base.is_empty() && (r_base.contains(&base) || base.contains(&r_base)) {
            15
        } else {
            -40
        };
        score += if !art.is_empty() && art == r_art {
            30
        } else if !first_artist.is_empty() && (r_art.contains(&first_artist) || art.contains(&r_art)) {
            15
        } else {
            -50
        };
        if !alb.is_empty() && !r_alb.is_empty() {
            score += if alb == r_alb {
                35
            } else if r_alb.contains(&alb) || alb.contains(&r_alb) {
                20
            } else {
                0
            };
        }
        if duration_s > 0 && dur > 0.0 {
            let d = (dur - duration_s as f64).abs();
            score += if d < 0.5 { 25 } else if d <= 1.0 { 20 } else { 15 };
        }
        let theirs = format!("{} {}", s("track_name").to_lowercase(), s("album_name").to_lowercase());
        for m in MODIFIERS {
            if theirs.contains(m) && !mine.contains(&m) {
                score -= 30;
            }
        }
        if score > 0 && best.as_ref().is_none_or(|(b, _)| score > *b) {
            best = Some((score, url.to_string()));
        }
    }
    best.map(|(_, u)| u)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTML: &str = r#"<tt xmlns="http://www.w3.org/ns/ttml" lrc:timing="Word"><head></head><body dur="6:09.629"><div begin="31.405" end="48.310"><p begin="31.405" end="33.728" ttm:agent="v1"><span begin="31.405" end="31.702">Like</span> <span begin="31.702" end="31.934">the</span> <span begin="32.785" end="33.728">Phoenix</span></p><p begin="39.985" end="43.950"><span begin="39.985" end="40.318">What</span> <span begin="43.192" end="43.475">uh-</span><span begin="43.475" end="43.950">uh</span><span ttm:role="x-bg"><span begin="43.5" end="44">(ooh)</span></span></p></div><div begin="49.590" end="1:05.655"><p begin="49.590" end="57.110"><span begin="49.590" end="50.838">We&apos;ve</span> <span begin="51.030" end="51.230">come</span></p></div></body></tt>"#;

    #[test]
    fn ttml_por_palabras_con_estrofas() {
        let l = parse_ttml("t", TTML, "BiniLyrics").unwrap();
        assert_eq!(l.sync_type, "SYLLABLE_SYNCED");
        let words: Vec<&str> = l.lines.iter().map(|x| x.words.as_str()).collect();
        assert_eq!(words, ["Like the Phoenix", "What uh-uh", "", "We've come"]);
        // Cada palabra con el espacio que la sigue; las de fondo, fuera.
        let s = &l.lines[0].syllables;
        assert_eq!((s[0].start_ms, s[0].chars, s[1].chars, s[2].chars), (31405, 5, 4, 7));
        assert_eq!(l.lines[1].syllables.iter().map(|s| s.chars).collect::<Vec<_>>(), [5, 3, 2]);
        // La pausa entre estrofas empieza al acabar el renglón anterior.
        assert_eq!(l.lines[2].start_ms, 43950);
        assert_eq!(l.lines[3].start_ms, 49590);
        assert_eq!(l.lines[0].end_ms, Some(33728));
        assert!(l.synced());
    }

    #[test]
    fn ttml_por_renglones() {
        let t = r#"<tt><body><div><p begin="0:12.5" end="0:15">Hola &amp; adiós</p><p begin="1:02:03.25" end="1:02:05">Otra</p></div></body></tt>"#;
        let l = parse_ttml("t", t, "BiniLyrics").unwrap();
        assert_eq!(l.sync_type, "LINE_SYNCED");
        assert_eq!(l.lines[0].words, "Hola & adiós");
        assert_eq!(l.lines[0].start_ms, 12500);
        assert_eq!(l.lines[1].start_ms, 3_723_250);
        assert!(l.lines[0].syllables.is_empty());
    }

    #[test]
    fn json_de_lyrics_plus() {
        let v: Value = serde_json::from_str(
            r#"{"type":"Word","lyrics":[
                {"time":31405,"duration":2323,"text":"Like the","syllabus":[{"time":31405,"duration":297,"text":"Like "},{"time":31702,"duration":232,"text":"the"},{"time":31800,"duration":100,"text":"(oh)","isBackground":true}],"element":{"songPartIndex":0}},
                {"time":35935,"duration":2120,"text":"All ends","syllabus":[{"time":35935,"text":"All "},{"time":36322,"text":"ends"}],"element":{"songPartIndex":0}},
                {"time":49590,"duration":7520,"text":"We've come","syllabus":[],"element":{"songPartIndex":1}}]}"#,
        )
        .unwrap();
        let l = parse_kpoe("t", &v, "Musixmatch").unwrap();
        assert_eq!(l.sync_type, "SYLLABLE_SYNCED");
        let words: Vec<&str> = l.lines.iter().map(|x| x.words.as_str()).collect();
        assert_eq!(words, ["Like the", "All ends", "", "We've come"]);
        assert_eq!(l.lines[0].syllables.len(), 2);
        assert_eq!(l.lines[0].end_ms, Some(33728));
        assert_eq!(l.lines[2].start_ms, 38055);
        assert!(l.lines[3].syllables.is_empty());
    }

    #[test]
    fn gana_el_orden_no_la_prisa() {
        // Llegó antes la de Musixmatch por renglones (hueco 4), pero BiniLyrics por sílabas aún
        // no respondió: no gana nadie todavía; se puede ir enseñando la 4.
        let mut got: Vec<Option<Option<&str>>> = vec![None, None, None, None, Some(Some("mxm-renglones")), None, None];
        assert_eq!(winner(&got), None);
        assert_eq!(best_so_far(&got), Some((4, &"mxm-renglones")));
        // BiniLyrics por sílabas la tiene: gana ya, aunque falten las demás.
        got[0] = Some(Some("bini-silabas"));
        assert_eq!(winner(&got), Some(&"bini-silabas"));
        // Sin sílabas ni palabras: la de BiniLyrics por renglones en cuanto las dos de delante
        // dicen que no, aunque falten LRCLIB y el resto.
        let got: Vec<Option<Option<&str>>> = vec![Some(None), Some(None), Some(Some("bini-renglones")), None, None, None, None];
        assert_eq!(winner(&got), Some(&"bini-renglones"));
        // Ninguna: nada.
        let got: Vec<Option<Option<&str>>> = vec![Some(None); 7];
        assert_eq!(winner(&got), None);
    }

    #[test]
    fn titulo_para_buscar() {
        assert_eq!(search_title("Get Lucky (feat. Pharrell Williams & Nile Rodgers)"), "Get Lucky");
        assert_eq!(search_title("Bohemian Rhapsody - Remastered 2011"), "Bohemian Rhapsody");
        assert_eq!(search_title("¿Y hace falta que te diga?"), "¿Y hace falta que te diga?");
    }

    #[test]
    fn elegir_en_binilyrics() {
        let r: Vec<Value> = serde_json::from_str(
            r#"[{"track_name":"Get Lucky (feat. Pharrell Williams) [Radio Edit]","artist_name":"Daft Punk","album_name":"Get Lucky - Single","duration":248,"timing_type":"word","lyricsUrl":"radio"},
                {"track_name":"Get Lucky","artist_name":"Daft Punk, Pharrell Williams, Nile Rodgers","album_name":"Random Access Memories","duration":370,"timing_type":"word","lyricsUrl":"album"},
                {"track_name":"Get Lucky (Drumless Edition)","artist_name":"Daft Punk","album_name":"Random Access Memories (Drumless Edition)","duration":370,"timing_type":"line","lyricsUrl":"drumless"},
                {"track_name":"Get Lucky","artist_name":"Someone Else","album_name":"Covers","duration":369,"timing_type":"word","lyricsUrl":"cover"}]"#,
        )
        .unwrap();
        let pick = |dur, timing| pick_lrcred(&r, "Get Lucky (feat. Pharrell Williams & Nile Rodgers)", "Daft Punk, Pharrell Williams", "Random Access Memories", dur, timing);
        assert_eq!(pick(369, "word").as_deref(), Some("album"));
        // Por renglones solo está la «Drumless Edition» (misma letra y tiempos): esa.
        assert_eq!(pick(369, "line").as_deref(), Some("drumless"));
        // Otro artista con el mismo título y duración no gana a la de Daft Punk.
        assert_ne!(pick(369, "word").as_deref(), Some("cover"));
        // Otra duración (la de la radio edit): esa.
        assert_eq!(pick(248, "word").as_deref(), Some("radio"));
    }
}
