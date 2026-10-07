//! Sonda de las mezclas de Spotify (operación `mix_probe` del modo de control, `Req::MixProbe`).
//!
//! Spotify guarda las playlists mezcladas («Mixear» en la app del teléfono: BPM por canción y una
//! transición por cada par) en sitios que no documenta: atributos de formato de la playlist y de
//! cada elemento, lentes y señales de playlist4, metadatos de context-resolve y extensiones de
//! extended-metadata que librespot 0.8 no conoce (de la 217 en adelante). Antes de escribir un
//! lector hay que saber cuáles de esos sitios contesta Spotify a una sesión de librespot: la sonda
//! los pide todos una vez y vuelca lo que llega a un informe de texto en %TEMP%. De ahí sale la
//! decisión del plan: 1A (metadatos de contexto), 1B (TRANSITION_DATA) o 1C (ninguno: solo las
//! mezclas propias de Nanofy).
//!
//! Aquí está todo lo que no habla con la red, para poder probarlo: el lector de protobuf sin
//! esquema, los resúmenes de cada respuesta y el filtro de lo sensible. El informe lleva solo
//! nombres de claves y valores recortados a 200 caracteres; las cabeceras de las peticiones nunca
//! se escriben, las claves con nombre de credencial se omiten y una última pasada (`scrub`) quita
//! cualquier línea que lo parezca.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use base64::Engine;
use librespot_protocol::playlist4_external::SelectedListContent;
use serde_json::Value;

/// Largo máximo de un valor (y de una clave) en el informe.
pub const MAX_VALUE: usize = 200;
/// Elementos de los que cada sección da el detalle; del resto, solo recuentos.
pub const DETAIL: usize = 10;
/// Canciones a las que se pide el BPM (AUDIO_ATTRIBUTES_V2), en un solo lote.
pub const BPM_MAX: usize = 100;
/// URIs de transición que se piden a TRANSITION_DATA y TRACK_PAIR_TRANSITION.
pub const TRANSITIONS_MAX: usize = 10;
/// Metadatos del contexto que se escriben (los de una playlist son pocos; es un tope por si acaso).
const CONTEXT_META_MAX: usize = 60;

/// Extensiones de extended-metadata de las mezclas, con su número en los protos de los clientes de
/// Spotify recientes. El enum de librespot 0.8 (sacado de la 1.2.52) acaba en la 195: estas solo se
/// pueden pedir por número.
pub const KIND_MIX_STATE: (i32, &str) = (225, "MIX_STATE");
pub const KIND_AUDIO_ATTRIBUTES: (i32, &str) = (222, "AUDIO_ATTRIBUTES_V2");
/// Las de una canción: pulsos, BPM y tonalidad, forma de onda en tres bandas, si se puede mezclar,
/// voz, secciones y los puntos de entrada antiguos (CUEPOINTS, este sí está en librespot).
pub const TRACK_KINDS: &[(i32, &str)] = &[
    (217, "BEATS"),
    (222, "AUDIO_ATTRIBUTES_V2"),
    (237, "THREEBAND_WAVEFORMS"),
    (219, "MIXABILITY"),
    (218, "VOCAL_ACTIVITY"),
    (261, "TRACK_SECTIONS"),
    (28, "CUEPOINTS"),
];
/// La de los datos de una transición propia de una playlist mezclada.
pub const KIND_TRANSITION_DATA: (i32, &str) = (244, "TRANSITION_DATA");
/// Las de una transición (`spotify:transition:…`).
pub const TRANSITION_KINDS: &[(i32, &str)] = &[KIND_TRANSITION_DATA, (126, "TRACK_PAIR_TRANSITION")];

/// Prefijos de las claves de metadatos que tocan a las mezclas: fundidos y transiciones
/// (`audio.*`, `automix.*`), recortes (`media.start_position`…), duración sustituida, velocidad y
/// las marcas de la playlist mezclada (`mix`, `mix-type`, `has-custom-transitions`…).
const MIX_PREFIXES: &[&str] = &[
    "audio.",
    "automix.",
    "media.",
    "duration_override",
    "has-custom-transitions",
    "custom_reporting_attribution",
    "item.speed",
    "playback_speed",
    "mix",
    "not-mixable",
    "can-view-transition",
];

/// Trozos de nombre (en minúsculas) de lo que podría ser una credencial. Una clave así no se
/// escribe, ni su valor.
const SENSITIVE: &[&str] = &["token", "bearer", "authoriz", "oauth", "secret", "password", "passwd", "credential", "cookie"];

/// Lo que ninguna línea del informe puede llevar (la comprobación de aceptación busca justo esto).
const NEVER_IN_REPORT: &[&str] = &["bearer", "token"];

/// ¿Clave de mezcla? (ver `MIX_PREFIXES`).
pub fn is_mix_key(k: &str) -> bool {
    MIX_PREFIXES.iter().any(|p| k.starts_with(p))
}

/// ¿Clave que describe la transición de una pista (y no solo que la playlist está mezclada)?
/// Fundidos con sus curvas, velocidad, uri o receta de la transición, recortes de inicio y fin.
/// Con alguna de estas en context-resolve, las transiciones se pueden leer (veredicto 1A).
pub fn is_transition_key(k: &str) -> bool {
    ["audio.fade", "audio.speed", "automix.", "media.start_position", "media.stop_position", "duration_override"]
        .iter()
        .any(|p| k.starts_with(p))
}

/// ¿Clave con pinta de tempo o tonalidad? El editor de mezclas de Spotify enseña el BPM y la
/// tonalidad de cada canción: si algún metadato los trae, el informe los saca aparte.
pub fn is_tempo_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    ["bpm", "tempo", "camelot", "beat", "speed"].iter().any(|w| k.contains(w))
        || k == "key"
        || k.ends_with(".key")
        || k.ends_with("_key")
        || k.ends_with("-key")
}

fn is_sensitive(s: &str) -> bool {
    let s = s.to_ascii_lowercase();
    SENSITIVE.iter().any(|w| s.contains(w))
}

/// Texto recortado a `max` caracteres (con «…» si se cortó) y sin caracteres de control, para que
/// cada dato quede en su línea.
fn clip_n(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).map(|c| if c.is_control() { ' ' } else { c }).collect();
    if s.chars().nth(max).is_some() {
        out.push('…');
    }
    out
}

/// Un valor del informe: como mucho `MAX_VALUE` caracteres.
pub fn clip(s: &str) -> String {
    clip_n(s, MAX_VALUE)
}

/// Nombre de una clave para el informe (las de nombre sensible, sin nombre).
fn key_name(k: &str) -> String {
    if is_sensitive(k) {
        "«clave sensible»".into()
    } else {
        clip(k)
    }
}

/// `clave=valor` para el informe: recortado y sin saltos; lo que tenga pinta de credencial (por la
/// clave o por el valor) no se escribe.
pub fn kv(k: &str, v: &str) -> String {
    if is_sensitive(k) {
        return "«clave sensible omitida»".into();
    }
    if is_sensitive(v) {
        return format!("{}=«oculto»", clip(k));
    }
    format!("{}={}", clip(k), clip(v))
}

/// `kv` y, si el valor de una clave de mezcla es un mensaje en base64 (así van las transiciones y
/// sus recetas), también lo que lleva dentro.
fn kv_mix(k: &str, v: &str) -> String {
    let mut s = kv(k, v);
    if is_mix_key(k) && !is_sensitive(k) && !is_sensitive(v) {
        if let Some(raw) = base64_message(v) {
            let _ = write!(s, " ⇒ {}", compact(&raw, 160));
        }
    }
    s
}

/// Las claves de mezcla y de tempo de unos metadatos, como `clave=valor` y en orden. Para el
/// registro del clúster de Connect (¿manda Spotify las transiciones a un dispositivo de terceros?).
pub fn mix_pairs<'a>(meta: impl IntoIterator<Item = (&'a String, &'a String)>) -> Vec<String> {
    let mut out: Vec<String> = meta
        .into_iter()
        .filter(|(k, _)| is_mix_key(k) || is_tempo_key(k))
        .map(|(k, v)| kv(k, v))
        .collect();
    out.sort();
    out
}

/// Última pasada antes de escribir: ninguna línea con pinta de credencial llega al disco, pase lo
/// que pase arriba (una clave nueva de Spotify, un error que copie una cabecera).
pub fn scrub(report: &str) -> String {
    let mut out = String::with_capacity(report.len());
    for line in report.lines() {
        let lower = line.to_ascii_lowercase();
        if NEVER_IN_REPORT.iter().any(|w| lower.contains(w)) {
            out.push_str("«línea omitida: parecía una credencial»");
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Bytes en hexadecimal.
pub fn hex(b: &[u8]) -> String {
    b.iter().fold(String::with_capacity(b.len() * 2), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// Los primeros bytes en hexadecimal (revisiones, plantillas): para reconocerlos, no para copiarlos.
fn hex_short(b: &[u8]) -> String {
    if b.len() <= 8 {
        hex(b)
    } else {
        format!("{}…", hex(&b[..8]))
    }
}

/// Título de una sección del informe.
pub fn section(title: &str) -> String {
    format!("\n== {title} ==")
}

/// Id de playlist (base62) de lo que se pase: el id, `spotify:playlist:<id>` o el enlace
/// `https://open.spotify.com/playlist/<id>?si=…`. `None` si no queda un id válido (va al nombre
/// del archivo del informe, así que solo letras y números).
pub fn playlist_id(s: &str) -> Option<String> {
    let last = s.trim().rsplit([':', '/']).next()?;
    let id = last.split(['?', '#']).next()?;
    (!id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric())).then(|| id.to_string())
}

/// Dónde queda el informe de una playlist.
pub fn report_path(id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("nanofy_mixprobe_{id}.txt"))
}

/// Escribe el informe (tras `scrub`) de una vez: primero a un temporal y luego se renombra, así
/// quien espera el archivo nunca lee uno a medias.
pub fn write_report(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, scrub(text))?;
    std::fs::rename(&tmp, path)
}

/// La decisión del plan con lo que ha contestado Spotify: 1A si los metadatos de contexto traen
/// claves de mezcla, 1B si TRANSITION_DATA trae datos (pueden ser las dos), 1C si ninguna.
pub fn decide(context_keys: bool, transition_data: bool) -> &'static str {
    match (context_keys, transition_data) {
        (true, true) => "1A+1B",
        (true, false) => "1A",
        (false, true) => "1B",
        (false, false) => "1C",
    }
}

/// Qué quiere decir cada veredicto.
pub fn explain(verdict: &str) -> &'static str {
    match verdict {
        "1A+1B" => "las transiciones se pueden leer por los metadatos de contexto y por TRANSITION_DATA",
        "1A" => "las transiciones llegan en los metadatos de context-resolve (audio.*, automix.*…): se pueden leer",
        "1B" => "TRANSITION_DATA (244) contesta: las transiciones se pueden leer por extended-metadata",
        _ => "no se pueden leer las mezclas de Spotify: solo mezclas propias de Nanofy",
    }
}

// --- Protobuf sin esquema ---------------------------------------------------------------------

/// Un campo protobuf leído sin su esquema, por su tipo de cable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wire<'a> {
    Varint(u64),
    I64(u64),
    Len(&'a [u8]),
    I32(u32),
}

/// Número de campo más alto que se acepta al adivinar si unos bytes son un mensaje: los de
/// Spotify son pequeños, y con un tope así unos bytes cualesquiera rara vez pasan por mensaje.
const MAX_FIELD: u32 = 2000;
/// Profundidad máxima de mensajes anidados en un volcado.
const MAX_DEPTH: usize = 5;
/// Veces que se escribe un campo repetido (pulsos, ventanas de una forma de onda); del resto, solo
/// cuántos hay.
const REPEAT_SHOWN: usize = 3;

fn read_varint(b: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*pos)?;
        *pos += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// Campos de un mensaje protobuf, en orden, sin su esquema. `None` si los bytes no son un mensaje
/// bien formado (los grupos, que ya no se usan, cuentan como mal formado).
pub fn wire_fields(b: &[u8]) -> Option<Vec<(u32, Wire<'_>)>> {
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < b.len() {
        let tag = read_varint(b, &mut pos)?;
        let field = u32::try_from(tag >> 3).ok().filter(|&f| f > 0 && f < (1 << 29))?;
        let value = match tag & 7 {
            0 => Wire::Varint(read_varint(b, &mut pos)?),
            1 => {
                let s = b.get(pos..pos.checked_add(8)?)?;
                pos += 8;
                Wire::I64(u64::from_le_bytes(s.try_into().ok()?))
            }
            2 => {
                let n = usize::try_from(read_varint(b, &mut pos)?).ok()?;
                let end = pos.checked_add(n)?;
                let s = b.get(pos..end)?;
                pos = end;
                Wire::Len(s)
            }
            5 => {
                let s = b.get(pos..pos.checked_add(4)?)?;
                pos += 4;
                Wire::I32(u32::from_le_bytes(s.try_into().ok()?))
            }
            _ => return None,
        };
        out.push((field, value));
    }
    Some(out)
}

/// ¿Parecen un mensaje? Bien formados, con algún campo y números de campo razonables.
fn as_message(b: &[u8]) -> Option<Vec<(u32, Wire<'_>)>> {
    wire_fields(b).filter(|f| !f.is_empty() && f.iter().all(|(n, _)| *n <= MAX_FIELD))
}

/// Un número de 32 o 64 bits fijos con valor razonable como coma flotante (así suele ir el BPM).
fn plausible_float(f: f64) -> bool {
    f.is_finite() && (f == 0.0 || (1e-3..=1e7).contains(&f.abs()))
}

fn fmt_float(f: f64) -> String {
    let s = format!("{f:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Bytes de un texto en base64, si lo parece (largo y con su alfabeto) y dentro hay un mensaje
/// protobuf: así guarda Spotify sus transiciones (TransitionData.transition, las recetas de
/// `automix.auto_transition_recipe`).
fn base64_message(text: &str) -> Option<Vec<u8>> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    if text.len() < 32 || !text.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'=' | b'-' | b'_')) {
        return None;
    }
    let raw = [STANDARD, URL_SAFE, STANDARD_NO_PAD, URL_SAFE_NO_PAD].iter().find_map(|e| e.decode(text).ok())?;
    as_message(&raw)?;
    Some(raw)
}

/// Escribe un mensaje sin esquema en una línea, con tope de largo mientras se construye (una forma
/// de onda entera serían megas de texto).
struct Render {
    out: String,
    cap: usize,
}

impl Render {
    fn full(&self) -> bool {
        self.out.len() >= self.cap
    }

    fn fields(&mut self, fields: &[(u32, Wire<'_>)], depth: usize) {
        self.out.push('{');
        let mut count: HashMap<u32, usize> = HashMap::new();
        let mut first = true;
        for (f, v) in fields {
            let c = count.entry(*f).or_default();
            *c += 1;
            if *c > REPEAT_SHOWN || self.full() {
                continue;
            }
            if !first {
                self.out.push(' ');
            }
            first = false;
            let _ = write!(self.out, "{f}=");
            self.value(v, depth);
        }
        let mut more: Vec<(u32, usize)> = count.into_iter().filter(|(_, c)| *c > REPEAT_SHOWN).map(|(f, c)| (f, c - REPEAT_SHOWN)).collect();
        more.sort_unstable();
        for (f, n) in more {
            let _ = write!(self.out, " …+{n}×{f}");
        }
        self.out.push('}');
    }

    fn value(&mut self, v: &Wire<'_>, depth: usize) {
        match *v {
            Wire::Varint(x) => {
                let _ = write!(self.out, "{x}");
            }
            Wire::I32(x) => self.number(f64::from(f32::from_bits(x)), u64::from(x)),
            Wire::I64(x) => self.number(f64::from_bits(x), x),
            Wire::Len(s) => self.len_value(s, depth),
        }
    }

    fn number(&mut self, f: f64, raw: u64) {
        if plausible_float(f) {
            let _ = write!(self.out, "{}f", fmt_float(f));
        } else {
            let _ = write!(self.out, "#{raw}");
        }
    }

    /// Un campo de longitud: texto, texto en base64 con un mensaje dentro, mensaje anidado o
    /// bytes (de estos, los primeros como coma flotante si lo parecen: pulsos, formas de onda).
    fn len_value(&mut self, s: &[u8], depth: usize) {
        if s.is_empty() {
            self.out.push_str("\"\"");
            return;
        }
        if let Some(text) = std::str::from_utf8(s).ok().filter(|t| !t.chars().any(char::is_control)) {
            if depth < MAX_DEPTH {
                if let Some(raw) = base64_message(text) {
                    // El texto también, por si el mensaje de dentro fuera casualidad.
                    let _ = write!(self.out, "\"{}\"→b64", clip_n(text, 40));
                    if let Some(fields) = as_message(&raw) {
                        self.fields(&fields, depth + 1);
                    }
                    return;
                }
            }
            let _ = write!(self.out, "\"{}\"", clip_n(text, 80));
            return;
        }
        if depth < MAX_DEPTH {
            if let Some(fields) = as_message(s) {
                self.fields(&fields, depth + 1);
                return;
            }
        }
        if let Ok(text) = std::str::from_utf8(s) {
            let _ = write!(self.out, "\"{}\"", clip_n(text, 80));
            return;
        }
        let _ = write!(self.out, "bytes[{}]:{}", s.len(), hex_short(s));
        if s.len() >= 8 && s.len() % 4 == 0 {
            let fl: Vec<f64> = s.chunks_exact(4).take(3).map(|c| f64::from(f32::from_le_bytes([c[0], c[1], c[2], c[3]]))).collect();
            if fl.iter().all(|&f| plausible_float(f)) {
                let _ = write!(self.out, " f32[{}…]", fl.iter().map(|&f| fmt_float(f)).collect::<Vec<_>>().join(", "));
            }
        }
    }
}

/// Unos campos sin esquema en una línea de como mucho `max` caracteres.
fn compact_fields(fields: &[(u32, Wire<'_>)], max: usize) -> String {
    let mut r = Render { out: String::new(), cap: max.saturating_mul(2).max(64) };
    r.fields(fields, 0);
    clip_n(&r.out, max)
}

/// Unos bytes protobuf sin esquema en una línea de como mucho `max` caracteres:
/// `{1=124f 2={1=5 2="8A"}}`. Para ver qué trae una extensión que librespot no conoce: números de
/// campo, textos, enteros, números fijos como coma flotante (el BPM suele ir así) y mensajes
/// anidados.
pub fn compact(b: &[u8], max: usize) -> String {
    match as_message(b) {
        Some(fields) => compact_fields(&fields, max),
        None => {
            let mut r = Render { out: String::new(), cap: max.saturating_mul(2).max(64) };
            r.len_value(b, 0);
            clip_n(&r.out, max)
        }
    }
}

/// Los campos que el proto de librespot no conoce (Spotify los añadió después de la 1.2.52), que
/// es justo donde pueden estar los de las mezclas.
fn unknown_line(u: &protobuf::UnknownFields) -> Option<String> {
    use protobuf::UnknownValueRef;
    let mut fields: Vec<(u32, Wire<'_>)> = u
        .iter()
        .map(|(n, v)| {
            let w = match v {
                UnknownValueRef::Fixed32(x) => Wire::I32(x),
                UnknownValueRef::Fixed64(x) => Wire::I64(x),
                UnknownValueRef::Varint(x) => Wire::Varint(x),
                UnknownValueRef::LengthDelimited(b) => Wire::Len(b),
            };
            (n, w)
        })
        .collect();
    if fields.is_empty() {
        return None;
    }
    // Por número de campo: el orden del almacén no es el del mensaje.
    fields.sort_by_key(|(n, _)| *n);
    Some(compact_fields(&fields, MAX_VALUE))
}

/// El BPM de AUDIO_ATTRIBUTES_V2 sin su esquema: el primer número de primer nivel con valor de
/// tempo (30–300), antes en coma flotante (32 o 64 bits) que entero. Es una suposición: el informe
/// la marca como «probable» y pone al lado el volcado entero.
pub fn bpm_guess(b: &[u8]) -> Option<f64> {
    let fields = as_message(b)?;
    let tempo = |x: f64| (30.0..=300.0).contains(&x).then_some(x);
    let float = fields.iter().find_map(|(_, v)| match *v {
        Wire::I32(x) => tempo(f64::from(f32::from_bits(x))),
        Wire::I64(x) => tempo(f64::from_bits(x)),
        _ => None,
    });
    float.or_else(|| {
        fields.iter().find_map(|(_, v)| match *v {
            Wire::Varint(x) => tempo(x as f64),
            _ => None,
        })
    })
}

/// Apunta `v` si es una uri de transición de Spotify (las transiciones propias de una playlist
/// mezclada; `spotify:core-auto-transition` no es una entidad que se pueda pedir).
fn note_transition(v: &str, transitions: &mut BTreeSet<String>) {
    if v.starts_with("spotify:transition:") && v.len() <= 200 {
        transitions.insert(v.to_string());
    }
}

/// `a ×3, b ×1`, o «ninguna».
fn histogram(h: &BTreeMap<String, usize>) -> String {
    if h.is_empty() {
        return "ninguna".into();
    }
    h.iter().map(|(k, n)| format!("{k} ×{n}")).collect::<Vec<_>>().join(", ")
}

fn signals(s: &[librespot_protocol::signal_model::Signal]) -> String {
    s.iter()
        .map(|x| {
            let mut t = key_name(&x.identifier);
            if !x.data.is_empty() {
                let _ = write!(t, " datos {}", compact(&x.data, 80));
            }
            if !x.client_payload.is_empty() {
                let _ = write!(t, " carga {} bytes", x.client_payload.len());
            }
            t
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// --- (a) playlist4 ----------------------------------------------------------------------------

/// Lo que se saca de la playlist4 cruda para las secciones siguientes.
#[derive(Default)]
pub struct ListFindings {
    /// (uri, item_id en hex) de cada elemento, en orden.
    pub items: Vec<(String, String)>,
    /// Atributos de formato de la lista (clave, valor).
    pub attrs: Vec<(String, String)>,
    /// Claves con pinta de tempo por uri de canción, ya como `clave=valor`.
    pub tempo: HashMap<String, Vec<String>>,
}

impl ListFindings {
    /// Las canciones de la lista (sin episodios ni archivos locales), en orden.
    pub fn track_uris(&self) -> Vec<String> {
        self.items.iter().filter(|(u, _)| u.starts_with("spotify:track:")).map(|(u, _)| u.clone()).collect()
    }

    /// Las marcas de mezcla de la playlist en sus atributos de formato (`mix=true`,
    /// `mix-type=only-auto`, `has-custom-transitions=true`), como `clave=valor`.
    pub fn mix_flags(&self) -> Vec<String> {
        self.attrs.iter().filter(|(k, _)| is_mix_key(k)).map(|(k, v)| kv(k, v)).collect()
    }
}

/// Resumen de una playlist4 cruda (SelectedListContent): atributos de formato de la lista, lentes
/// aplicadas, señales, campos que el proto de librespot no conoce y, de los primeros elementos,
/// item_id, atributos de formato, señales y lente de origen. Añade a `transitions` las uris de
/// transición que encuentre.
pub fn describe_list(msg: &SelectedListContent, out: &mut Vec<String>, transitions: &mut BTreeSet<String>) -> ListFindings {
    let mut f = ListFindings::default();
    let attrs = &msg.attributes;
    let contents = &msg.contents;
    out.push(format!(
        "revisión {} · largo {} · {} elementos recibidos desde la posición {}{}",
        hex_short(msg.revision()),
        msg.length(),
        contents.items.len(),
        contents.pos(),
        if contents.truncated() { " · truncada" } else { "" }
    ));
    out.push(format!("nombre: «{}»", clip(attrs.name())));
    out.push(format!("format: {}", if attrs.has_format() { clip(attrs.format()) } else { "(no viene)".into() }));
    out.push(format!("atributos de formato de la lista ({}):", attrs.format_attributes.len()));
    for a in &attrs.format_attributes {
        out.push(format!("  {}", kv_mix(a.key(), a.value())));
        f.attrs.push((a.key().to_string(), a.value().to_string()));
        note_transition(a.value(), transitions);
    }
    if attrs.has_sequence_context_template() {
        let t = attrs.sequence_context_template();
        out.push(format!("sequence_context_template: {} bytes {}", t.len(), compact(t, MAX_VALUE)));
    }
    if attrs.has_ai_curation_reference_id() {
        out.push(format!("ai_curation_reference_id: {} bytes", attrs.ai_curation_reference_id().len()));
    }
    if let Some(u) = unknown_line(attrs.special_fields.unknown_fields()) {
        out.push(format!("campos desconocidos de los atributos: {u}"));
    }
    if let Some(u) = unknown_line(msg.special_fields.unknown_fields()) {
        out.push(format!("campos desconocidos de la respuesta: {u}"));
    }
    if let Some(u) = unknown_line(contents.special_fields.unknown_fields()) {
        out.push(format!("campos desconocidos de la lista de elementos: {u}"));
    }
    match msg.applied_lenses.as_ref().filter(|l| !l.states.is_empty()) {
        Some(l) => {
            for s in &l.states {
                out.push(format!("lente aplicada: {} (revisión {})", key_name(&s.identifier), hex_short(&s.revision)));
            }
        }
        None => out.push("lentes aplicadas: ninguna".into()),
    }
    if !contents.available_signals.is_empty() {
        out.push(format!("señales de la lista: {}", signals(&contents.available_signals)));
    }
    if contents.has_continuation_token() {
        out.push("la lista sigue en otra página (hay continuación)".into());
    }

    let mut with_id = 0;
    let mut keys: BTreeMap<String, usize> = BTreeMap::new();
    let mut sigs: BTreeMap<String, usize> = BTreeMap::new();
    let mut lenses: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown: BTreeMap<String, usize> = BTreeMap::new();
    let mut child_templates = 0;
    let mut detail = Vec::new();
    for (i, it) in contents.items.iter().enumerate() {
        let a = &it.attributes;
        let uri = it.uri();
        let id_hex = hex(a.item_id());
        if a.has_item_id() {
            with_id += 1;
        }
        for fa in &a.format_attributes {
            *keys.entry(key_name(fa.key())).or_default() += 1;
            note_transition(fa.value(), transitions);
            if is_tempo_key(fa.key()) {
                f.tempo.entry(uri.to_string()).or_default().push(kv(fa.key(), fa.value()));
            }
        }
        for s in &a.available_signals {
            *sigs.entry(key_name(&s.identifier)).or_default() += 1;
        }
        if let Some(l) = a.source_lens.as_ref() {
            *lenses.entry(key_name(&l.identifier)).or_default() += 1;
        }
        for (n, _) in a.special_fields.unknown_fields().iter() {
            *unknown.entry(format!("campo {n}")).or_default() += 1;
        }
        if a.has_sequence_child_template() {
            child_templates += 1;
        }
        if i < DETAIL {
            let mut line = format!("  #{i} {} · item_id {}", clip(uri), if id_hex.is_empty() { "(no viene)" } else { &id_hex });
            let fmt: Vec<String> = a.format_attributes.iter().map(|x| kv_mix(x.key(), x.value())).collect();
            if !fmt.is_empty() {
                let _ = write!(line, " · formato [{}]", fmt.join("; "));
            }
            if !a.available_signals.is_empty() {
                let _ = write!(line, " · señales [{}]", signals(&a.available_signals));
            }
            if let Some(l) = a.source_lens.as_ref() {
                let _ = write!(line, " · lente {}", key_name(&l.identifier));
            }
            if a.has_sequence_child_template() {
                let _ = write!(line, " · plantilla {}", compact(a.sequence_child_template(), 120));
            }
            if let Some(u) = unknown_line(a.special_fields.unknown_fields()) {
                let _ = write!(line, " · desconocidos {u}");
            }
            detail.push(line);
        }
        f.items.push((uri.to_string(), id_hex));
    }
    let n = contents.items.len();
    out.push(format!("elementos con item_id: {with_id} de {n}"));
    out.push(format!("claves de formato por elemento: {}", histogram(&keys)));
    out.push(format!("señales por elemento: {}", histogram(&sigs)));
    out.push(format!("lentes de origen por elemento: {}", histogram(&lenses)));
    out.push(format!("campos desconocidos por elemento: {}", histogram(&unknown)));
    if child_templates > 0 {
        out.push(format!("elementos con sequence_child_template: {child_templates}"));
    }
    if !detail.is_empty() {
        out.push(format!("primeros {} elementos:", detail.len()));
        out.extend(detail);
    }
    f
}

// --- (b) context-resolve ----------------------------------------------------------------------

/// Lo que se saca de context-resolve.
#[derive(Default)]
pub struct ContextFindings {
    /// Uris de las pistas, en orden.
    pub tracks: Vec<String>,
    /// Pistas con alguna clave de mezcla en sus metadatos.
    pub with_mix_keys: usize,
    /// Pistas con alguna clave que describe su transición (`is_transition_key`).
    pub with_transition_keys: usize,
    /// Claves de mezcla del contexto (no de las pistas), como `clave=valor`.
    pub context_mix: Vec<String>,
    /// uid de pistas que coinciden con el item_id (hex) de algún elemento de la playlist4.
    pub uid_equal: usize,
    /// Pistas con uid comparadas.
    pub uid_compared: usize,
    /// Claves con pinta de tempo por uri de canción, ya como `clave=valor`.
    pub tempo: HashMap<String, Vec<String>>,
}

/// Un mapa JSON de texto a texto (los `metadata` de context-resolve), en orden; lo que no sea
/// texto, en JSON.
fn str_map(v: &Value) -> BTreeMap<String, String> {
    v.as_object()
        .map(|o| o.iter().map(|(k, x)| (k.clone(), x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string()))).collect())
        .unwrap_or_default()
}

fn opt_str(v: &Value) -> String {
    match v.as_str() {
        Some(s) if !s.is_empty() => format!("sí ({})", clip_n(s, 120)),
        _ => "no".into(),
    }
}

/// Resumen del JSON de context-resolve de la playlist, el mismo que lee Spirc: metadatos del
/// contexto y de cada página, claves de los metadatos de cada pista (las de mezcla con su valor) y
/// si el uid de cada pista es el item_id de playlist4 (así se emparejarían las transiciones con
/// las filas). `items`: (uri, item_id en hex) de la playlist4.
pub fn describe_context(v: &Value, items: &[(String, String)], out: &mut Vec<String>, transitions: &mut BTreeSet<String>) -> ContextFindings {
    let mut f = ContextFindings::default();
    let Some(obj) = v.as_object() else {
        out.push("la respuesta no es un objeto JSON".into());
        return f;
    };
    out.push(format!("claves de primer nivel: {}", obj.keys().map(|k| key_name(k)).collect::<Vec<_>>().join(", ")));
    let meta = str_map(&v["metadata"]);
    out.push(format!("metadatos del contexto ({}):", meta.len()));
    for (k, val) in &meta {
        note_transition(val, transitions);
        if is_mix_key(k) {
            f.context_mix.push(kv_mix(k, val));
        }
    }
    for (k, val) in meta.iter().take(CONTEXT_META_MAX) {
        out.push(format!("  {}", kv_mix(k, val)));
    }
    if meta.len() > CONTEXT_META_MAX {
        out.push(format!("  … y {} más", meta.len() - CONTEXT_META_MAX));
    }
    let pages = v["pages"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    out.push(format!("páginas: {}", pages.len()));
    let item_ids: HashSet<String> = items.iter().map(|(_, h)| h.to_ascii_lowercase()).filter(|h| !h.is_empty()).collect();
    let mut all_keys: BTreeMap<String, usize> = BTreeMap::new();
    let mut mix_keys: BTreeMap<String, usize> = BTreeMap::new();
    let mut detail = Vec::new();
    let mut n = 0usize;
    for (p, page) in pages.iter().enumerate() {
        let tracks = page["tracks"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        if p < 3 {
            let pmeta = str_map(&page["metadata"]);
            out.push(format!(
                "  página {p}: {} pistas · page_url {} · next_page_url {} · metadatos [{}]",
                tracks.len(),
                opt_str(&page["page_url"]),
                opt_str(&page["next_page_url"]),
                pmeta.iter().map(|(k, x)| kv(k, x)).collect::<Vec<_>>().join("; ")
            ));
        }
        for t in tracks {
            let uri = t["uri"].as_str().unwrap_or("");
            let uid = t["uid"].as_str().unwrap_or("");
            let tm = str_map(&t["metadata"]);
            let mut mix = Vec::new();
            for (k, val) in &tm {
                *all_keys.entry(key_name(k)).or_default() += 1;
                note_transition(val, transitions);
                if is_mix_key(k) {
                    *mix_keys.entry(key_name(k)).or_default() += 1;
                    mix.push(kv_mix(k, val));
                }
                if is_tempo_key(k) && !uri.is_empty() {
                    f.tempo.entry(uri.to_string()).or_default().push(kv(k, val));
                }
            }
            if !mix.is_empty() {
                f.with_mix_keys += 1;
            }
            if tm.keys().any(|k| is_transition_key(k)) {
                f.with_transition_keys += 1;
            }
            // Solo las filas que también llegaron en la playlist4: si esta vino truncada (lista
            // larga), las de más allá saldrían como «distinto» sin serlo.
            if !uid.is_empty() && !item_ids.is_empty() && n < items.len() {
                f.uid_compared += 1;
                if item_ids.contains(&uid.to_ascii_lowercase()) {
                    f.uid_equal += 1;
                }
            }
            if n < DETAIL {
                let others: Vec<String> = tm.keys().filter(|k| !is_mix_key(k)).map(|k| key_name(k)).collect();
                let mut line = format!("  #{n} {} · uid {}", clip(uri), if uid.is_empty() { "(no viene)".into() } else { clip(uid) });
                if let Some((_, h)) = items.get(n) {
                    let _ = write!(line, " (item_id #{n} {})", if h.is_empty() { "no viene" } else { h });
                }
                let _ = write!(line, " · mezcla [{}] · otras claves [{}]", mix.join("; "), others.join(", "));
                detail.push(line);
            }
            if !uri.is_empty() {
                f.tracks.push(uri.to_string());
            }
            n += 1;
        }
    }
    out.push(format!("pistas en total: {n}"));
    out.push(format!("claves de metadatos de las pistas: {}", histogram(&all_keys)));
    out.push(format!("claves de mezcla de las pistas: {} (en {} de {n} pistas)", histogram(&mix_keys), f.with_mix_keys));
    out.push(format!("pistas con claves de transición (audio.fade*, automix.*, media.start/stop…): {} de {n}", f.with_transition_keys));
    out.push(if f.uid_compared > 0 {
        format!("uid == item_id de playlist4: {} de {}", f.uid_equal, f.uid_compared)
    } else {
        "uid == item_id de playlist4: no se puede comparar (falta uno de los dos)".into()
    });
    if !detail.is_empty() {
        out.push(format!("primeras {} pistas:", detail.len()));
        out.extend(detail);
    }
    f
}

// --- (c) extended-metadata --------------------------------------------------------------------

/// Una respuesta de extended-metadata: por tipo, el estado del proveedor y, por entidad, su
/// código, el type_url y el tamaño de lo que trae (y su volcado, para las `detail` primeras).
/// Devuelve (uri, tipo, bytes) de las entidades que traen datos.
pub fn describe_batch(bytes: &[u8], detail: usize, out: &mut Vec<String>) -> Result<Vec<(String, i32, Vec<u8>)>, String> {
    use librespot_protocol::extended_metadata::BatchedExtensionResponse;
    use protobuf::Message;
    let resp = BatchedExtensionResponse::parse_from_bytes(bytes).map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    if resp.extended_metadata.is_empty() {
        out.push(format!("   la respuesta no trae ningún tipo ({} bytes)", bytes.len()));
    }
    for arr in &resp.extended_metadata {
        let kind = arr.extension_kind.value();
        let h = &arr.header;
        out.push(format!(
            "   tipo {kind}: error del proveedor {} · extension_type {} · {} entidades",
            h.provider_error_status,
            h.extension_type.value(),
            arr.extension_data.len()
        ));
        let mut empty = 0;
        for (i, d) in arr.extension_data.iter().enumerate() {
            let (type_url, value) = d.extension_data.as_ref().map(|a| (a.type_url.as_str(), a.value.as_slice())).unwrap_or(("", &[]));
            if value.is_empty() {
                empty += 1;
            } else {
                found.push((d.entity_uri.clone(), kind, value.to_vec()));
            }
            if i < detail {
                out.push(format!(
                    "     {} · estado {} · {} · {} bytes{}",
                    clip(&d.entity_uri),
                    d.header.status_code,
                    if type_url.is_empty() { "(sin type_url)".into() } else { clip(type_url) },
                    value.len(),
                    if value.is_empty() { String::new() } else { format!(" · {}", compact(value, MAX_VALUE)) }
                ));
            }
        }
        if arr.extension_data.len() > detail {
            out.push(format!("     … {} entidades más; sin datos, {empty} en total", arr.extension_data.len() - detail));
        }
    }
    Ok(found)
}

// --- (e) audio-analysis -----------------------------------------------------------------------

/// Lo que importa a un mezclador del JSON de audio-analysis: tempo, tonalidad, compás, sonoridad
/// y cuántos pulsos, compases y secciones trae.
pub fn describe_analysis(v: &Value) -> Vec<String> {
    let t = &v["track"];
    let vals: Vec<String> = ["tempo", "tempo_confidence", "key", "key_confidence", "mode", "time_signature", "loudness", "duration"]
        .iter()
        .filter_map(|k| t.get(*k).map(|x| format!("{k}={}", clip_n(&x.to_string(), 40))))
        .collect();
    let mut out = vec![if vals.is_empty() {
        let keys = v.as_object().map(|o| o.keys().map(|k| key_name(k)).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        format!("sin «track» (claves: {keys})")
    } else {
        vals.join(" · ")
    }];
    let counts: Vec<String> = ["bars", "beats", "tatums", "sections", "segments"]
        .iter()
        .filter_map(|k| v[*k].as_array().map(|a| format!("{k} {}", a.len())))
        .collect();
    if !counts.is_empty() {
        out.push(counts.join(" · "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use protobuf::Message;

    /// Mensaje protobuf a mano: (campo, tipo de cable, bytes del valor ya codificados).
    fn tag(field: u32, wire: u8) -> Vec<u8> {
        varint((u64::from(field) << 3) | u64::from(wire))
    }
    fn varint(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return out;
            }
            out.push(b | 0x80);
        }
    }
    fn f_varint(field: u32, v: u64) -> Vec<u8> {
        [tag(field, 0), varint(v)].concat()
    }
    fn f_float(field: u32, v: f32) -> Vec<u8> {
        [tag(field, 5), v.to_le_bytes().to_vec()].concat()
    }
    fn f_double(field: u32, v: f64) -> Vec<u8> {
        [tag(field, 1), v.to_le_bytes().to_vec()].concat()
    }
    fn f_len(field: u32, v: &[u8]) -> Vec<u8> {
        [tag(field, 2), varint(v.len() as u64), v.to_vec()].concat()
    }

    #[test]
    fn wire_reads_every_type() {
        let msg = [f_varint(1, 300), f_float(2, 124.0), f_double(3, 0.5), f_len(4, b"8A")].concat();
        let fields = wire_fields(&msg).unwrap();
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[0], (1, Wire::Varint(300)));
        assert_eq!(fields[1], (2, Wire::I32(124.0f32.to_bits())));
        assert_eq!(fields[2], (3, Wire::I64(0.5f64.to_bits())));
        assert_eq!(fields[3], (4, Wire::Len(b"8A")));
    }

    #[test]
    fn wire_rejects_malformed() {
        // Largo que se sale del mensaje.
        assert!(wire_fields(&[tag(1, 2), vec![10, b'a']].concat()).is_none());
        // Campo 0.
        assert!(wire_fields(&[0x00, 0x01]).is_none());
        // Grupo (tipo 3), ya no se usa.
        assert!(wire_fields(&tag(1, 3)).is_none());
        // Varint sin terminar y fijo de 32 bits cortado.
        assert!(wire_fields(&[0x08, 0x80]).is_none());
        assert!(wire_fields(&[tag(1, 5), vec![1, 2]].concat()).is_none());
        // Vacío: un mensaje sin campos.
        assert_eq!(wire_fields(&[]).unwrap().len(), 0);
    }

    #[test]
    fn compact_renders_nested_floats_and_text() {
        let camelot = [f_len(1, b"8A"), f_len(2, b"#ff0000")].concat();
        let key = [f_varint(1, 5), f_varint(2, 1), f_len(3, &camelot)].concat();
        let msg = [f_float(1, 124.0), f_len(2, &key)].concat();
        assert_eq!(compact(&msg, 200), "{1=124f 2={1=5 2=1 3={1=\"8A\" 2=\"#ff0000\"}}}");
    }

    #[test]
    fn compact_collapses_repeated_fields_and_clips() {
        let msg: Vec<u8> = (0..10).flat_map(|i| f_varint(1, i)).collect();
        assert_eq!(compact(&msg, 200), "{1=0 1=1 1=2 …+7×1}");
        let long = vec![b'x'; 500];
        let s = compact(&f_len(1, &long), 50);
        assert_eq!(s.chars().count(), 51);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn compact_opens_base64_messages() {
        use base64::engine::general_purpose::STANDARD;
        // Una transición: pista A, pista B y una duración, como TransitionData.transition.
        let inner = [f_len(10, b"spotify:track:aaaaaaaaaaaaaaaaaaaaaa"), f_len(11, b"spotify:track:bbbbbbbbbbbbbbbbbbbbbb"), f_varint(5, 8000)].concat();
        let b64 = STANDARD.encode(&inner);
        let s = compact(&f_len(6, b64.as_bytes()), 400);
        assert!(s.contains("→b64{10=\"spotify:track:aaa"), "{s}");
        assert!(s.contains("5=8000"), "{s}");
        // Un id base62 no es base64 de nada: se queda como texto.
        assert_eq!(compact(&f_len(1, b"37i9dQZF1DZ06evO01g8Cs"), 200), "{1=\"37i9dQZF1DZ06evO01g8Cs\"}");
    }

    #[test]
    fn mix_values_in_base64_are_opened() {
        use base64::engine::general_purpose::STANDARD;
        let inner = [f_len(1, b"spotify:track:aaaaaaaaaaaaaaaaaaaaaa"), f_varint(3, 8000), f_varint(4, 4000)].concat();
        let v = STANDARD.encode(&inner);
        let s = kv_mix("automix.auto_transition_recipe", &v);
        assert!(s.starts_with("automix.auto_transition_recipe="), "{s}");
        assert!(s.contains(" ⇒ {1=\"spotify:track:aaa"), "{s}");
        assert!(s.contains("3=8000 4=4000}"), "{s}");
        // Fuera de las claves de mezcla, el valor tal cual.
        assert_eq!(kv_mix("image_url", &v), kv("image_url", &v));
    }

    #[test]
    fn compact_shows_packed_floats_as_bytes() {
        let packed: Vec<u8> = [0.5f32, 1.0, 1.5, 2.0].iter().flat_map(|f| f.to_le_bytes()).collect();
        let s = compact(&packed, 200);
        assert!(s.starts_with("bytes[16]:"), "{s}");
        assert!(s.contains("f32[0.5, 1, 1.5…]"), "{s}");
    }

    #[test]
    fn bpm_guess_prefers_floats_in_tempo_range() {
        let key = [f_varint(1, 5), f_varint(2, 1)].concat();
        assert_eq!(bpm_guess(&[f_float(1, 124.0), f_len(2, &key)].concat()), Some(124.0));
        assert_eq!(bpm_guess(&[f_varint(3, 128), f_double(1, 97.5)].concat()), Some(97.5));
        // Solo enteros: el que cae en el rango de tempo, no la tonalidad (5).
        assert_eq!(bpm_guess(&[f_varint(1, 5), f_varint(2, 120)].concat()), Some(120.0));
        assert_eq!(bpm_guess(&f_varint(1, 5)), None);
        assert_eq!(bpm_guess(b"\xff\xff"), None);
    }

    #[test]
    fn kv_hides_credentials_and_clips() {
        assert_eq!(kv("authorization", "Bearer abc"), "«clave sensible omitida»");
        assert_eq!(kv("x", "Bearer abc"), "x=«oculto»");
        assert_eq!(kv("audio.fade_in_duration", "3000"), "audio.fade_in_duration=3000");
        let long = "a".repeat(300);
        let line = kv("k", &long);
        assert_eq!(line.chars().count(), 2 + MAX_VALUE + 1);
        assert_eq!(kv("k", "a\nb"), "k=a b");
    }

    #[test]
    fn scrub_drops_credential_lines() {
        let out = scrub("ok 1\nAuthorization: Bearer xyz\nplaylist_token=abc\nok 2");
        assert_eq!(out, "ok 1\n«línea omitida: parecía una credencial»\n«línea omitida: parecía una credencial»\nok 2\n");
        assert!(!out.to_ascii_lowercase().contains("bearer"));
        assert!(!out.to_ascii_lowercase().contains("token"));
    }

    #[test]
    fn key_classes() {
        for k in ["audio.fade_in_duration", "automix.transition_uri", "media.start_position", "duration_override", "has-custom-transitions", "mix", "mix-type"] {
            assert!(is_mix_key(k), "{k}");
        }
        for k in ["context_uri", "track_player", "added_at"] {
            assert!(!is_mix_key(k), "{k}");
        }
        for k in ["bpm", "audio.tempo", "camelot_key", "key", "musical.key", "beats_per_bar", "item.speed"] {
            assert!(is_tempo_key(k), "{k}");
        }
        for k in ["monkey", "keyword", "uid"] {
            assert!(!is_tempo_key(k), "{k}");
        }
        // Que la playlist esté mezclada no es poder leer sus transiciones.
        for k in ["audio.fade_in_curves", "audio.speed_automation", "automix.transition_uri", "media.start_position", "duration_override"] {
            assert!(is_transition_key(k), "{k}");
        }
        for k in ["has-custom-transitions", "mix", "custom_reporting_attribution", "audio.tempo"] {
            assert!(!is_transition_key(k), "{k}");
        }
        let meta: HashMap<String, String> =
            [("automix.transition_uri", "spotify:transition:x"), ("bpm", "124"), ("uid", "1")].into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        assert_eq!(mix_pairs(&meta), vec!["automix.transition_uri=spotify:transition:x".to_string(), "bpm=124".to_string()]);
    }

    #[test]
    fn playlist_id_from_any_form() {
        assert_eq!(playlist_id("37i9dQZF1DZ06evO01g8Cs").as_deref(), Some("37i9dQZF1DZ06evO01g8Cs"));
        assert_eq!(playlist_id("spotify:playlist:abc123").as_deref(), Some("abc123"));
        assert_eq!(playlist_id(" https://open.spotify.com/playlist/abc123?si=zz ").as_deref(), Some("abc123"));
        // Solo el último trozo, y solo letras y números: nunca sale de %TEMP% ni lleva puntos.
        assert_eq!(playlist_id("../etc").as_deref(), Some("etc"));
        assert_eq!(playlist_id("abc.txt"), None);
        assert_eq!(playlist_id("spotify:playlist:"), None);
        assert_eq!(playlist_id(""), None);
        assert_eq!(playlist_id("a b"), None);
    }

    #[test]
    fn verdicts() {
        assert_eq!(decide(true, false), "1A");
        assert_eq!(decide(false, true), "1B");
        assert_eq!(decide(true, true), "1A+1B");
        assert_eq!(decide(false, false), "1C");
        assert!(explain("1C").contains("solo mezclas propias"));
    }

    fn sample_list() -> SelectedListContent {
        use librespot_protocol::lens_model::LensState;
        use librespot_protocol::playlist4_external::{AppliedLenses, FormatListAttribute, Item};
        use librespot_protocol::signal_model::Signal;
        let attr = |k: &str, v: &str| {
            let mut a = FormatListAttribute::new();
            a.set_key(k.into());
            a.set_value(v.into());
            a
        };
        let mut msg = SelectedListContent::new();
        msg.set_revision(vec![1, 2, 3]);
        msg.set_length(3);
        let at = msg.attributes.mut_or_insert_default();
        at.set_name("mix".into());
        at.format_attributes.push(attr("mix", "true"));
        at.format_attributes.push(attr("has-custom-transitions", "true"));
        at.special_fields.mut_unknown_fields().add_varint(77, 1);
        let mut lenses = AppliedLenses::new();
        let mut st = LensState::new();
        st.identifier = "mix".into();
        lenses.states.push(st);
        msg.applied_lenses = protobuf::MessageField::some(lenses);
        let contents = msg.contents.mut_or_insert_default();
        // Obligatorios en playlist4 (proto2): sin ellos no se puede serializar.
        contents.set_pos(0);
        contents.set_truncated(false);
        for (i, uri) in ["spotify:track:aaa", "spotify:episode:bbb", "spotify:track:ccc"].iter().enumerate() {
            let mut it = Item::new();
            it.set_uri(uri.to_string());
            let a = it.attributes.mut_or_insert_default();
            a.set_item_id(vec![0xab, i as u8]);
            if i == 0 {
                a.format_attributes.push(attr("automix.transition_uri", "spotify:transition:t1"));
                a.format_attributes.push(attr("bpm", "124"));
                let mut s = Signal::new();
                s.identifier = "set-transition".into();
                a.available_signals.push(s);
            }
            contents.items.push(it);
        }
        msg
    }

    #[test]
    fn describe_list_finds_flags_ids_and_transitions() {
        let msg = sample_list();
        let mut out = Vec::new();
        let mut tr = BTreeSet::new();
        let f = describe_list(&msg, &mut out, &mut tr);
        let text = out.join("\n");
        assert_eq!(f.items.len(), 3);
        assert_eq!(f.items[0], ("spotify:track:aaa".to_string(), "ab00".to_string()));
        assert_eq!(f.track_uris(), vec!["spotify:track:aaa".to_string(), "spotify:track:ccc".to_string()]);
        assert_eq!(f.mix_flags(), vec!["mix=true".to_string(), "has-custom-transitions=true".to_string()]);
        assert_eq!(f.tempo.get("spotify:track:aaa").unwrap(), &vec!["bpm=124".to_string()]);
        assert!(tr.contains("spotify:transition:t1"));
        assert!(text.contains("nombre: «mix»"), "{text}");
        assert!(text.contains("lente aplicada: mix"), "{text}");
        assert!(text.contains("campos desconocidos de los atributos: {77=1}"), "{text}");
        assert!(text.contains("elementos con item_id: 3 de 3"), "{text}");
        assert!(text.contains("señales por elemento: set-transition ×1"), "{text}");
        assert!(text.contains("#0 spotify:track:aaa · item_id ab00 · formato [automix.transition_uri=spotify:transition:t1; bpm=124] · señales [set-transition]"), "{text}");
        // Y sobrevive al viaje de ida y vuelta por el cable (así llega de Spotify).
        let again = SelectedListContent::parse_from_bytes(&msg.write_to_bytes().unwrap()).unwrap();
        let mut out2 = Vec::new();
        describe_list(&again, &mut out2, &mut BTreeSet::new());
        assert_eq!(out, out2);
    }

    #[test]
    fn describe_context_reads_mix_keys_and_uids() {
        let v: Value = serde_json::json!({
            "uri": "spotify:playlist:x",
            "metadata": {"format_list_type": "mix", "has-custom-transitions": "true"},
            "pages": [{
                "tracks": [
                    {"uri": "spotify:track:aaa", "uid": "AB00", "metadata": {
                        "automix.transition_uri": "spotify:transition:t2",
                        "audio.fade_out_start_time": "180000",
                        "audio.tempo": "124.0",
                        "added_at": "1"
                    }},
                    {"uri": "spotify:track:ccc", "uid": "zz", "metadata": {}}
                ]
            }]
        });
        let items = vec![("spotify:track:aaa".to_string(), "ab00".to_string()), ("spotify:track:ccc".to_string(), "ab02".to_string())];
        let mut out = Vec::new();
        let mut tr = BTreeSet::new();
        let f = describe_context(&v, &items, &mut out, &mut tr);
        let text = out.join("\n");
        assert_eq!(f.tracks.len(), 2);
        assert_eq!(f.with_mix_keys, 1);
        assert_eq!(f.with_transition_keys, 1);
        assert_eq!(f.context_mix, vec!["has-custom-transitions=true".to_string()]);
        assert_eq!((f.uid_equal, f.uid_compared), (1, 2));
        assert!(tr.contains("spotify:transition:t2"));
        assert_eq!(f.tempo.get("spotify:track:aaa").unwrap(), &vec!["audio.tempo=124.0".to_string()]);
        assert!(text.contains("uid == item_id de playlist4: 1 de 2"), "{text}");
        assert!(text.contains("en 1 de 2 pistas"), "{text}");
        assert!(text.contains("otras claves [added_at]"), "{text}");
    }

    #[test]
    fn describe_batch_reports_unknown_kinds() {
        use librespot_protocol::entity_extension_data::EntityExtensionData;
        use librespot_protocol::extended_metadata::{BatchedExtensionResponse, EntityExtensionDataArray};
        let mut resp = BatchedExtensionResponse::new();
        let mut arr = EntityExtensionDataArray::new();
        arr.extension_kind = protobuf::EnumOrUnknown::from_i32(222);
        let mut d = EntityExtensionData::new();
        d.entity_uri = "spotify:track:aaa".into();
        d.header.mut_or_insert_default().status_code = 200;
        let any = d.extension_data.mut_or_insert_default();
        any.type_url = "type.googleapis.com/spotify.x.AudioAttributes".into();
        any.value = f_float(1, 124.0);
        arr.extension_data.push(d);
        let mut empty = EntityExtensionData::new();
        empty.entity_uri = "spotify:track:ccc".into();
        empty.header.mut_or_insert_default().status_code = 404;
        arr.extension_data.push(empty);
        resp.extended_metadata.push(arr);
        let bytes = resp.write_to_bytes().unwrap();
        let mut out = Vec::new();
        let found = describe_batch(&bytes, 1, &mut out).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "spotify:track:aaa");
        assert_eq!(found[0].1, 222);
        assert_eq!(bpm_guess(&found[0].2), Some(124.0));
        let text = out.join("\n");
        assert!(text.contains("tipo 222"), "{text}");
        assert!(text.contains("estado 200 · type.googleapis.com/spotify.x.AudioAttributes · 5 bytes · {1=124f}"), "{text}");
        assert!(text.contains("… 1 entidades más; sin datos, 1 en total"), "{text}");
        assert!(describe_batch(b"\xff", 1, &mut out).is_err());
    }

    #[test]
    fn describe_analysis_picks_tempo() {
        let v: Value = serde_json::json!({"track": {"tempo": 123.98, "key": 9, "mode": 1, "time_signature": 4}, "beats": [1, 2, 3], "sections": [1]});
        let out = describe_analysis(&v);
        assert_eq!(out[0], "tempo=123.98 · key=9 · mode=1 · time_signature=4");
        assert_eq!(out[1], "beats 3 · sections 1");
        assert!(describe_analysis(&serde_json::json!({"error": 1}))[0].starts_with("sin «track»"));
    }

    #[test]
    fn report_is_written_whole_and_clean() {
        let dir = std::env::temp_dir().join(format!("nanofy_mixprobe_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("informe.txt");
        write_report(&path, "== x ==\nAuthorization: Bearer abc\nfin").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.to_ascii_lowercase().contains("bearer"));
        assert!(text.ends_with("fin\n"));
        assert!(!path.with_extension("txt.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(report_path("abc").ends_with("nanofy_mixprobe_abc.txt"));
    }
}
