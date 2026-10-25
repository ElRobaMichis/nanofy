//! Traducciones. El texto en español es la clave: en el código va `t("Canciones que te gustan")`
//! o, con datos, `tr!("{} canciones", n)`. Cada idioma es un CSV de dos columnas (español y
//! traducción) en `lang/<código>.csv`: los del repositorio van dentro del ejecutable y los de una
//! carpeta `lang/` junto a él se añaden o los sustituyen (para probar una traducción sin compilar).
//! Lo que falte en un idioma se ve en español. Ver `lang/README.md` para traducir.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

/// Los idiomas que van dentro del ejecutable (código y CSV).
const BUILT_IN: &[(&str, &str)] = &[("en", include_str!("../lang/en.csv"))];

/// Un idioma: código («en»), su nombre en ese idioma («English»), sus separadores de miles y
/// de decimales y sus traducciones.
pub struct Language {
    pub code: String,
    pub name: String,
    thousands: char,
    decimal: char,
    map: HashMap<String, String>,
}

impl Language {
    fn spanish() -> Language {
        Language { code: "es".into(), name: "Español".into(), thousands: '.', decimal: ',', map: HashMap::new() }
    }
}

struct Catalog {
    /// El primero es el español (sin traducciones: el texto es la clave).
    langs: Vec<Language>,
}

static CATALOG: OnceLock<Catalog> = OnceLock::new();
/// Índice del idioma en uso en `Catalog::langs` (0: español).
static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// Lee un CSV (comillas dobles para campos con comas, saltos de línea o comillas, que se
/// escriben dobladas): filas de campos.
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else if c == '\r' && chars.peek() == Some(&'\n') {
                // Un salto CRLF (Excel, git en Windows) dentro de un texto cuenta como «\n».
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                if row.iter().any(|f| !f.is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        if row.iter().any(|f| !f.is_empty()) {
            rows.push(row);
        }
    }
    rows
}

/// Un idioma de su CSV: la primera fila es la cabecera («es,en»); las filas con clave `@name`,
/// `@thousands` y `@decimal` dan su nombre y sus separadores (si faltan, los del español). Las
/// traducciones vacías no cuentan (se verá el español).
fn language(code: &str, text: &str) -> Language {
    let mut lang = Language { code: code.to_string(), name: code.to_string(), ..Language::spanish() };
    for (i, row) in parse_csv(text).into_iter().enumerate() {
        if i == 0 || row.len() < 2 {
            continue;
        }
        let (key, value) = (row[0].clone(), row[1].clone());
        let first = value.chars().next();
        match key.as_str() {
            "@name" => lang.name = value,
            "@thousands" => lang.thousands = first.unwrap_or(lang.thousands),
            "@decimal" => lang.decimal = first.unwrap_or(lang.decimal),
            _ if !value.trim().is_empty() => {
                lang.map.insert(key, value);
            }
            _ => {}
        }
    }
    lang
}

/// Carga los idiomas (una vez, al arrancar): los de dentro y los de `lang/` junto al ejecutable.
pub fn init() {
    CATALOG.get_or_init(|| {
        let mut langs = vec![Language::spanish()];
        for (code, text) in BUILT_IN {
            langs.push(language(code, text));
        }
        let dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("lang")));
        if let Some(entries) = dir.and_then(|d| std::fs::read_dir(d).ok()) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(code) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
                if path.extension().and_then(|e| e.to_str()) != Some("csv") || code == "es" {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                let lang = language(&code, &text);
                log::info!("[idioma] {} ({}) desde {}: {} textos", lang.name, code, path.display(), lang.map.len());
                match langs.iter_mut().find(|l| l.code == code) {
                    Some(l) => *l = lang,
                    None => langs.push(lang),
                }
            }
        }
        Catalog { langs }
    });
}

fn catalog() -> &'static Catalog {
    init();
    CATALOG.get().expect("catálogo de idiomas")
}

/// Idiomas disponibles: (código, nombre).
pub fn languages() -> Vec<(String, String)> {
    catalog().langs.iter().map(|l| (l.code.clone(), l.name.clone())).collect()
}

/// El idioma del sistema (código de dos letras), si es uno de los disponibles.
fn system_language() -> Option<usize> {
    let loc = sys_locale::get_locale()?;
    let code = loc.split(['-', '_']).next()?.to_ascii_lowercase();
    catalog().langs.iter().position(|l| l.code == code)
}

/// Elige el idioma: «auto» (el del sistema; si no está, español) o un código.
pub fn set(code: &str) {
    let c = catalog();
    let i = if code == "auto" || code.is_empty() { system_language().unwrap_or(0) } else { c.langs.iter().position(|l| l.code == code).unwrap_or(0) };
    CURRENT.store(i, Ordering::Relaxed);
}

/// Código del idioma en uso («es», «en»…): también se pide así a Spotify y a Deezer.
pub fn current() -> &'static str {
    let c = catalog();
    &c.langs[CURRENT.load(Ordering::Relaxed).min(c.langs.len() - 1)].code
}

fn current_language() -> &'static Language {
    let c = catalog();
    &c.langs[CURRENT.load(Ordering::Relaxed).min(c.langs.len() - 1)]
}

/// Separador de miles del idioma en uso («1.196.073» en español, «1,196,073» en inglés).
pub fn thousands_sep() -> char {
    current_language().thousands
}

/// Un número ya escrito con punto decimal («4.7»), con la coma o el punto del idioma en uso.
pub fn decimal(s: String) -> String {
    let d = current_language().decimal;
    if d == '.' {
        s
    } else {
        s.replace('.', &d.to_string())
    }
}

/// El texto traducido al idioma en uso (o el mismo, en español o si falta).
pub fn t(s: &'static str) -> &'static str {
    let c = catalog();
    let i = CURRENT.load(Ordering::Relaxed);
    if i == 0 {
        return s;
    }
    c.langs.get(i).and_then(|l| l.map.get(s)).map(String::as_str).unwrap_or(s)
}

/// Como `t` para un texto que no es `'static` (el de un dato que coincide con una clave).
pub fn t_owned(s: &str) -> String {
    let c = catalog();
    let i = CURRENT.load(Ordering::Relaxed);
    c.langs.get(i).and_then(|l| l.map.get(s)).cloned().unwrap_or_else(|| s.to_string())
}

/// Mensajes que el código reconoce por su texto (en español) y por eso no se traducen al crearlos:
/// sus plantillas, para que el script de traducciones las recoja; `t_message` los traduce al
/// mostrarlos.
#[allow(dead_code)]
pub const MATCHED: &[&str] = &[
    crate::tk!("no has iniciado sesión"),
    crate::tk!("Spotify está limitando las peticiones; inténtalo de nuevo en unos segundos."),
    crate::tk!("Spotify está limitando las peticiones; espera unos segundos"),
    crate::tk!("Spotify está limitando las peticiones (reintenta en {} s)"),
    crate::tk!("Spotify ha agotado la cuota de la biblioteca; vuelve en {}. La reproducción sigue funcionando."),
    crate::tk!("respuesta inesperada: {}"),
];

/// Traduce un mensaje ya construido (un aviso o un error que llega en español): si es una clave,
/// su traducción; si encaja en una plantilla con `{}` («… reintenta en {} s»), esa plantilla
/// traducida con los mismos valores; si no, tal cual.
pub fn t_message(s: &str) -> String {
    let c = catalog();
    let Some(lang) = c.langs.get(CURRENT.load(Ordering::Relaxed)).filter(|_| CURRENT.load(Ordering::Relaxed) != 0) else {
        return s.to_string();
    };
    if let Some(v) = lang.map.get(s) {
        return v.clone();
    }
    for (key, value) in &lang.map {
        if !key.contains("{}") {
            continue;
        }
        if let Some(args) = match_template(key, s) {
            return fill(value, &args);
        }
    }
    s.to_string()
}

/// Los valores de `s` en los huecos `{}` de la plantilla `key`, si encaja.
fn match_template(key: &str, s: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = key.split("{}").collect();
    let mut rest = s.strip_prefix(parts[0])?;
    let mut args = Vec::new();
    for (i, part) in parts.iter().enumerate().skip(1) {
        let last = i == parts.len() - 1;
        let at = if last {
            if !rest.ends_with(part) {
                return None;
            }
            rest.len() - part.len()
        } else if part.is_empty() {
            return None;
        } else {
            rest.find(part)?
        };
        args.push(rest[..at].to_string());
        rest = &rest[at + part.len()..];
    }
    Some(args)
}

/// Rellena una plantilla traducida: `{}` toma los valores por orden y `{0}`, `{1}`… por número
/// (para que una traducción cambie el orden).
pub fn fill(template: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut next = 0usize;
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('}') {
            Some(j) if after[..j].chars().all(|c| c.is_ascii_digit()) => {
                let idx = if j == 0 {
                    next += 1;
                    next - 1
                } else {
                    after[..j].parse().unwrap_or(0)
                };
                out.push_str(args.get(idx).map(String::as_str).unwrap_or(""));
                rest = &after[j + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Texto traducido con datos: `tr!("{} canciones", n)` (la plantilla en español es la clave).
#[macro_export]
macro_rules! tr {
    ($s:literal) => { $crate::i18n::t($s).to_string() };
    ($s:literal, $($a:expr),+ $(,)?) => {
        $crate::i18n::fill($crate::i18n::t($s), &[$(format!("{}", $a)),+])
    };
}

/// Marca un texto traducible donde no se puede llamar a `t` (constantes, listas fijas): da el
/// mismo texto y el script de traducciones lo recoge. Se traduce al usarlo, con `t(CONST)`.
#[macro_export]
macro_rules! tk {
    ($s:literal) => {
        $s
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_con_comillas_comas_y_saltos() {
        let rows = parse_csv("es,en\n\"Hola, mundo\",\"Hello, world\"\n\"Dijo \"\"sí\"\"\",\"Said \"\"yes\"\"\"\nDos\nlíneas,x\n\"a\nb\",\"c\nd\"\n");
        assert_eq!(rows[1], vec!["Hola, mundo", "Hello, world"]);
        assert_eq!(rows[2], vec!["Dijo \"sí\"", "Said \"yes\""]);
        assert_eq!(rows[3], vec!["Dos"]);
        assert_eq!(rows[5], vec!["a\nb", "c\nd"]);
        let crlf = parse_csv("es,en\r\n\"a\r\nb\",\"c\r\nd\"\r\n");
        assert_eq!(crlf[1], vec!["a\nb", "c\nd"]);
    }

    #[test]
    fn plantillas() {
        assert_eq!(fill("{} canciones", &["12".into()]), "12 canciones");
        assert_eq!(fill("{1} de {0}", &["A".into(), "B".into()]), "B de A");
        assert_eq!(fill("{x} y {}", &["1".into()]), "{x} y 1");
    }

    #[test]
    fn mensajes_con_plantilla() {
        assert_eq!(match_template("Spotify está limitando (reintenta en {} s)", "Spotify está limitando (reintenta en 12 s)"), Some(vec!["12".to_string()]));
        assert_eq!(match_template("{} de {}", "A de B"), Some(vec!["A".to_string(), "B".to_string()]));
        assert_eq!(match_template("Hola {}", "Adiós x"), None);
    }

    #[test]
    fn separadores_desde_csv() {
        let l = language("en", "es,en
@thousands,\",\"
@decimal,.
");
        assert_eq!((l.thousands, l.decimal), (',', '.'));
        let l = language("xx", "es,xx
@name,X
");
        assert_eq!((l.thousands, l.decimal), ('.', ','));
    }

    #[test]
    fn idioma_desde_csv() {
        let l = language("en", "es,en\n@name,English\nInicio,Home\nVacía,\n");
        assert_eq!(l.name, "English");
        assert_eq!(l.map.get("Inicio").map(String::as_str), Some("Home"));
        assert!(!l.map.contains_key("Vacía"));
    }
}
