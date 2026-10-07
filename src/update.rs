//! Aviso de versiones nuevas: consulta la última release publicada en GitHub y la compara con
//! la versión compilada. Una sola petición sin autenticación (límite de GitHub: 60 por hora e IP),
//! en un hilo aparte; el resultado llega a la interfaz por el bus.
//!
//! La instalación se prepara en segundo plano (`stage`): descarga comprobada con el tamaño y el
//! SHA-256 que publica GitHub, el ejecutable nuevo junto al actual y una autoprueba
//! (`--self-test`) antes de darlo por bueno.
//!
//! La sustitución (`apply_staged`) va con la ventana aún abierta o al arrancar (`on_launch`),
//! nunca al cerrar. La versión instalada queda a prueba hasta que funciona un rato
//! (`mark_healthy`); si no llega a arrancar, se vuelve sola a la anterior.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::bus::{Msg, UiTx};

/// Repositorio del que se leen las releases.
pub const REPO: &str = "ElRobaMichis/nanofy";
/// Página de releases (enlace de respaldo si no hay zip para esta plataforma).
pub const RELEASES_URL: &str = "https://github.com/ElRobaMichis/nanofy/releases";

/// Versión en ejecución. `NANOFY_VERSION` permite fingir otra para probar el aviso
/// (por ejemplo `NANOFY_VERSION=0.9.0` hace que la release actual parezca nueva).
pub fn current_version() -> String {
    std::env::var("NANOFY_VERSION")
        .ok()
        .filter(|v| parse_version(v).is_some())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

#[derive(Clone, Debug, PartialEq)]
pub struct UpdateInfo {
    /// Versión publicada, sin la «v» (por ejemplo `1.2.0`).
    pub version: String,
    /// Página de la release en GitHub (notas y todos los archivos).
    pub page_url: String,
    /// Notas completas de la release (Markdown), sin la línea dirigida a 1.1–1.3. El aviso
    /// flotante solo enseña el principio (`notes_preview`).
    pub notes: String,
    /// Zip de esta plataforma, si la release ya lo incluye.
    pub asset: Option<Asset>,
}

/// Caracteres de las notas que caben en el aviso flotante.
const NOTES_PREVIEW_CHARS: usize = 400;

impl UpdateInfo {
    /// Principio de las notas para el aviso flotante (que solo enseña sus primeras líneas).
    pub fn notes_preview(&self) -> String {
        if self.notes.chars().count() > NOTES_PREVIEW_CHARS {
            self.notes.chars().take(NOTES_PREVIEW_CHARS).collect::<String>() + "…"
        } else {
            self.notes.clone()
        }
    }

    /// Las primeras `n` líneas con texto de las notas, ya sin Markdown (aviso flotante).
    pub fn preview_lines(&self, n: usize) -> Vec<String> {
        note_lines(&self.notes_preview())
            .into_iter()
            .filter_map(|l| match l {
                NoteLine::Heading(t) | NoteLine::Text(t) => Some(t),
                NoteLine::Bullet(t) => Some(format!("• {t}")),
                NoteLine::Gap => None,
            })
            .take(n)
            .collect()
    }
}

/// Una línea de las notas de la release tal como la pinta la interfaz.
#[derive(Clone, Debug, PartialEq)]
pub enum NoteLine {
    Heading(String),
    Bullet(String),
    Text(String),
    /// Separación entre párrafos (una o varias líneas en blanco seguidas, o una raya `---`).
    Gap,
}

/// Las notas vienen del mensaje de la etiqueta (Markdown sencillo escrito a mano) y la interfaz
/// no pinta Markdown: se quedan los títulos (`#`), las viñetas (`-`, `*`, `+`) y el texto, sin
/// los `**`, `__` ni acentos graves que se verían tal cual.
pub fn note_lines(notes: &str) -> Vec<NoteLine> {
    let plain = |s: &str| s.replace("**", "").replace("__", "").replace('`', "").trim().to_string();
    let mut out: Vec<NoteLine> = Vec::new();
    for raw in notes.lines() {
        let line = raw.trim();
        let rule = line.len() >= 3 && (line.bytes().all(|b| b == b'-') || line.bytes().all(|b| b == b'*') || line.bytes().all(|b| b == b'_'));
        if line.is_empty() || rule {
            if !matches!(out.last(), None | Some(NoteLine::Gap)) {
                out.push(NoteLine::Gap);
            }
            continue;
        }
        let hashes = line.bytes().take_while(|b| *b == b'#').count();
        let after = &line[hashes..];
        let item = if hashes > 0 && (after.is_empty() || after.starts_with(' ')) {
            NoteLine::Heading(plain(after))
        } else if let Some(b) = ["-", "*", "+"].iter().find_map(|p| line.strip_prefix(p).filter(|r| r.is_empty() || r.starts_with(' '))) {
            NoteLine::Bullet(plain(b))
        } else {
            NoteLine::Text(plain(line))
        };
        // Un «#» o un «-» sueltos no dicen nada.
        if !matches!(&item, NoteLine::Heading(t) | NoteLine::Bullet(t) | NoteLine::Text(t) if t.is_empty()) {
            out.push(item);
        }
    }
    if matches!(out.last(), Some(NoteLine::Gap)) {
        out.pop();
    }
    out
}

/// Zip de la release para esta plataforma, con lo que GitHub publica de él para poder
/// comprobar la descarga.
#[derive(Clone, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    /// Tamaño en bytes según GitHub (0 = desconocido).
    pub size: u64,
    /// SHA-256 que GitHub calcula al subir el archivo (campo `digest`). Las releases anteriores
    /// a junio de 2025 o un servidor de pruebas pueden no traerlo.
    pub sha256: Option<[u8; 32]>,
}

// A mano para que el registro muestre el digest en hexadecimal y no como 32 números sueltos.
impl std::fmt::Debug for Asset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let digest = self.sha256.map(|d| hex(&d));
        f.debug_struct("Asset")
            .field("name", &self.name)
            .field("url", &self.url)
            .field("size", &self.size)
            .field("sha256", &digest)
            .finish()
    }
}

/// Texto del aviso cuando la release no trae zip para este sistema.
pub const NO_ASSET_TEXT: &str = "Esta versión aún no tiene descarga para tu sistema.";
/// Motivo del fallo cuando el disco está lleno (`ErrorKind::StorageFull`).
pub const DISK_FULL_TEXT: &str = "No hay espacio suficiente en el disco para la actualización.";

/// Lo que dice el aviso de un fallo: una frase según el tipo (qué pasó y qué puede hacer el
/// usuario) y, aparte y en pequeño, el motivo real si añade algo. `detail` es el de
/// `Stage::Failed`; `current`, la versión en uso.
pub fn fail_message(kind: FailKind, detail: &str, current: &str) -> (String, Option<String>) {
    let detail = detail.trim();
    let headline = match kind {
        FailKind::Network => "No se pudo descargar la actualización. Revisa tu conexión.".to_string(),
        FailKind::Corrupt => "La descarga llegó dañada y se descartó.".to_string(),
        // «Disk» es también cualquier otro error al escribir (permisos, una carpeta que ya no
        // está): solo con el disco lleno se dice que falta espacio.
        FailKind::Disk if detail == DISK_FULL_TEXT => return ("No hay espacio suficiente en el disco.".to_string(), None),
        FailKind::Disk => "No se pudo guardar la actualización en el disco.".to_string(),
        FailKind::Blocked if cfg!(target_os = "windows") => format!("Windows o tu antivirus bloqueó la versión nueva. Sigues usando la {current}."),
        FailKind::Blocked => format!("El sistema no dejó arrancar la versión nueva. Sigues usando la {current}."),
        // `swap_failure_text` ya es la frase entera, con el motivo entre paréntesis.
        FailKind::Swap if !detail.is_empty() => return (detail.to_string(), None),
        FailKind::Swap => "No se pudo sustituir el ejecutable.".to_string(),
        FailKind::NoAsset => NO_ASSET_TEXT.to_string(),
    };
    let extra = match split_reason(detail) {
        // La misma frase con el motivo entre paréntesis (la autoprueba, el SHA-256): solo el motivo.
        Some((sentence, reason)) if sentence == headline => Some(reason.to_string()),
        _ if detail.is_empty() || detail == headline => None,
        _ => Some(detail.to_string()),
    };
    (headline, extra)
}

/// «Frase (motivo) resto» → («Frase resto», «motivo»): del primer paréntesis al último.
fn split_reason(s: &str) -> Option<(String, &str)> {
    let open = s.find('(')?;
    let close = s.rfind(')').filter(|c| *c > open)?;
    let reason = s[open + 1..close].trim();
    if reason.is_empty() {
        return None;
    }
    Some((format!("{}{}", s[..open].trim_end(), &s[close + 1..]), reason))
}

/// Por qué falló la instalación: decide qué ofrece el aviso después («Reintentar» o, si no hay
/// nada que instalar, la página de la release).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailKind {
    /// Sin conexión, GitHub respondió con un error o la descarga se cortó.
    Network,
    /// La descarga o el zip no son lo que publica la release (tamaño, SHA-256 o contenido).
    Corrupt,
    /// No se pudo escribir la descarga o el ejecutable nuevo (disco lleno u otro error local).
    Disk,
    /// El sistema o el antivirus no dejan arrancar el ejecutable nuevo (falló la autoprueba).
    Blocked,
    /// No se pudo sustituir el ejecutable en uso.
    Swap,
    /// No hay zip que la app pueda instalar sola en este sistema.
    NoAsset,
}

/// Por qué la app no puede actualizarse sola desde donde está.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveReason {
    /// La carpeta del ejecutable no deja escribir (Program Files, una carpeta protegida…).
    ReadOnly,
    /// Se abrió desde dentro del zip: el explorador lo descomprime en una carpeta temporal, y lo
    /// que se instalase ahí no es lo que se abre la próxima vez.
    RunningFromZip,
}

impl MoveReason {
    /// Texto del aviso; `dir` es la carpeta del ejecutable.
    pub fn text(self, dir: &Path) -> String {
        match self {
            MoveReason::ReadOnly => format!(
                "Nanofy está en una carpeta donde no puede actualizarse solo ({}). Muévelo, por ejemplo a {}, y a partir de ahí se actualizará solo.",
                dir.display(),
                if cfg!(target_os = "windows") { r"%LOCALAPPDATA%\Programs\Nanofy" } else { "~/.local/bin" }
            ),
            MoveReason::RunningFromZip => {
                "Estás abriendo Nanofy desde dentro del zip. Descomprímelo en una carpeta y ábrelo desde ahí.".to_string()
            }
        }
    }
}

/// Estado de la instalación de una versión nueva (lo enseñan el aviso y Ajustes).
#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    /// Nada que instalar.
    Idle,
    /// Hay una versión nueva que se puede instalar.
    Available,
    Downloading { done: u64, total: Option<u64> },
    /// Descarga comprobada: extrayendo el ejecutable y probándolo.
    Preparing,
    /// El ejecutable nuevo está junto al actual (`staged`), probado y con su `nanofy.update.json`.
    Ready { version: String, staged: PathBuf },
    /// Desde la carpeta `dir` la app no puede sustituirse sola.
    NeedsMove { dir: PathBuf, reason: MoveReason },
    /// `detail` es el motivo real, en el idioma de la interfaz: se enseña tal cual.
    Failed { kind: FailKind, detail: String },
}

impl Stage {
    pub fn label(&self) -> String {
        match self {
            Stage::Idle | Stage::Available => String::new(),
            Stage::Downloading { done, total: Some(t) } if *t > 0 => format!("Descargando… {} %", (done * 100 / t).min(100)),
            Stage::Downloading { done, .. } => format!("Descargando… {:.1} MB", *done as f64 / 1e6),
            Stage::Preparing => "Preparando la actualización…".to_string(),
            // Lista no es instalada: se instala al pulsar «Reiniciar» o al abrir Nanofy otra vez.
            Stage::Ready { version, .. } => format!("Nanofy {version} está lista"),
            Stage::NeedsMove { dir, reason } => reason.text(dir),
            Stage::Failed { detail, .. } => detail.clone(),
        }
    }

    /// Nombre corto para el modo de control.
    pub fn name(&self) -> &'static str {
        match self {
            Stage::Idle => "idle",
            Stage::Available => "available",
            Stage::Downloading { .. } => "downloading",
            Stage::Preparing => "preparing",
            Stage::Ready { .. } => "ready",
            Stage::NeedsMove { .. } => "needs_move",
            Stage::Failed { .. } => "failed",
        }
    }

    /// Instalación en marcha (no se puede lanzar otra ni tocar la versión que se instala).
    pub fn busy(&self) -> bool {
        matches!(self, Stage::Downloading { .. } | Stage::Preparing | Stage::Ready { .. })
    }
}

/// Barra y texto de la descarga en el aviso: la fracción hecha, si se sabe el total, y
/// «{pct} % · {hecho} de {total} MB» (sin total, solo lo descargado).
pub fn download_progress(done: u64, total: Option<u64>) -> (Option<f32>, String) {
    let mb = |b: u64| b as f64 / 1e6;
    match total.filter(|t| *t > 0) {
        Some(t) => {
            let done = done.min(t);
            (Some(done as f32 / t as f32), format!("{} % · {:.1} de {:.1} MB", done * 100 / t, mb(done), mb(t)))
        }
        None => (None, format!("{:.1} MB", mb(done))),
    }
}

/// `true` si en esta plataforma la app puede sustituirse a sí misma (ejecutable suelto).
pub fn can_self_install() -> bool {
    cfg!(any(target_os = "windows", target_os = "linux"))
}

/// Lo que decide si la app puede prepararse sola las versiones nuevas (`auto_update_allowed`).
pub struct AutoGuard<'a> {
    /// Compilación de depuración.
    pub debug: bool,
    /// Sesión de capturas (`--side none`): no guarda nada al salir.
    pub ephemeral: bool,
    /// Modo de control de las pruebas (`--control`).
    pub control: bool,
    /// `NANOFY_UPDATE_URL` apunta a un servidor de releases de este equipo.
    pub test_server: bool,
    /// Ejecutable en uso.
    pub exe: Option<&'a Path>,
}

/// «Actualizar automáticamente» solo vale para una copia instalada de verdad. Una versión
/// preparada sola se instala al abrir Nanofy la próxima vez, así que una compilación de
/// desarrollo (de depuración, o la de target\release con la que se trabaja), una sesión de
/// capturas o el modo de control de las pruebas (las versiones de referencia de la contraprueba)
/// se sustituirían a sí mismas por la última release. En el modo de control sí se permite con un
/// servidor de releases local: es justo lo que quieren probar las pruebas de actualización.
/// Cuando no vale, la versión nueva se avisa como con la opción apagada.
pub fn auto_update_allowed(g: &AutoGuard) -> bool {
    !g.debug && !g.ephemeral && (!g.control || g.test_server) && g.exe.is_some_and(|e| !is_dev_build_path(e))
}

/// El ejecutable es el que deja cargo (`target\release`, `target\debug`, también con la
/// plataforma en medio: `target\x86_64-pc-windows-gnu\release`).
pub fn is_dev_build_path(exe: &Path) -> bool {
    let parts: Vec<String> = exe
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    let profile = |s: &String| s == "release" || s == "debug";
    parts.iter().enumerate().any(|(i, p)| {
        p == "target" && (parts.get(i + 1).is_some_and(profile) || (parts.get(i + 1).is_some_and(|t| t.contains('-')) && parts.get(i + 2).is_some_and(profile)))
    })
}

/// `NANOFY_UPDATE_URL` es un servidor de releases de este equipo (pruebas de actualización).
pub fn test_server_configured() -> bool {
    std::env::var("NANOFY_UPDATE_URL").is_ok_and(|u| is_local_url(&u))
}

/// Nombre del ejecutable dentro del zip de la release para esta plataforma.
fn binary_name_in_zip() -> &'static str {
    if cfg!(target_os = "windows") {
        "nanofy.exe"
    } else {
        "nanofy"
    }
}

/// Primeros bytes de un ejecutable de esta plataforma (PE, Mach-O de 64 bits o ELF).
fn exe_magic() -> &'static [u8] {
    if cfg!(target_os = "windows") {
        b"MZ"
    } else if cfg!(target_os = "macos") {
        &[0xCF, 0xFA, 0xED, 0xFE]
    } else {
        b"\x7fELF"
    }
}

/// Ejecutable en uso, leído una sola vez y antes de renombrar nada (`on_launch` lo pide lo
/// primero): en Linux `current_exe` sigue al archivo renombrado y, tras una sustitución, ya no
/// diría dónde tiene que estar Nanofy.
static TARGET: OnceLock<Option<PathBuf>> = OnceLock::new();

pub fn target_exe() -> Option<PathBuf> {
    TARGET.get_or_init(|| std::env::current_exe().ok()).clone()
}

/// `nanofy.<name>.exe` junto a `target` (en Linux, `nanofy.<name>`).
fn exe_sibling(target: &Path, name: &str) -> PathBuf {
    target.with_extension(if cfg!(target_os = "windows") { format!("{name}.exe") } else { name.to_string() })
}

/// El ejecutable anterior tras instalar una versión: se guarda hasta que la nueva funciona, para
/// poder volver a él.
fn old_exe_path(target: &Path) -> PathBuf {
    exe_sibling(target, "old")
}

/// Una versión que no llegaba a arrancar, apartada al volver a la anterior.
fn bad_exe_path(target: &Path) -> PathBuf {
    exe_sibling(target, "bad")
}

/// `nanofy.update.applied.json`: la versión recién instalada mientras está a prueba.
fn applied_path(target: &Path) -> PathBuf {
    target.with_extension("update.applied.json")
}

/// `nanofy.update.bad.json`: la versión que no arrancaba y se devolvió.
fn bad_meta_path(target: &Path) -> PathBuf {
    target.with_extension("update.bad.json")
}

fn lock_path(target: &Path) -> PathBuf {
    target.with_extension("update.lock")
}

/// Ejecutable nuevo ya probado, en la carpeta del actual: así la sustitución es un simple
/// renombrado, que solo funciona dentro del mismo disco. Visible a propósito: un ejecutable
/// oculto que aparece solo es justo lo que vigilan los antivirus.
pub fn staged_exe_path(exe: &Path) -> PathBuf {
    exe.with_extension(if cfg!(target_os = "windows") { "update.exe" } else { "update" })
}

/// `nanofy.update.json`, junto al ejecutable preparado. Se escribe lo último, así que solo existe
/// si ese ejecutable está entero y pasó la autoprueba.
pub fn staged_meta_path(exe: &Path) -> PathBuf {
    exe.with_extension("update.json")
}

/// Contenido de `nanofy.update.json`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StagedMeta {
    pub version: String,
    /// Ejecutable al que sustituye (el que lo preparó).
    pub target: PathBuf,
    pub exe_size: u64,
    /// SHA-256 del ejecutable preparado, en hexadecimal.
    pub exe_sha256: String,
    /// Arranques en los que no se pudo sustituir el ejecutable: cada intento retrasa el arranque
    /// hasta unos 2 s, así que tras `MAX_SWAP_FAILURES` se descarta.
    #[serde(default)]
    pub swap_failures: u8,
}

/// Plazo de la autoprueba. Arrancar y responder lleva milisegundos; si un antivirus la retiene
/// más que esto, se da por bloqueada.
const SELF_TEST_LIMIT: Duration = Duration::from_secs(15);
/// Tamaño mínimo del ejecutable (el de verdad pesa unos 16 MB): uno menor no es Nanofy.
const MIN_EXE_SIZE: u64 = 1_000_000;
/// Tope de la descarga cuando la release no dice cuánto pesa el zip (unos 8 MB): un servidor
/// que no deja de mandar no debe llenar el disco.
const MAX_DOWNLOAD: u64 = 256 << 20;

/// Turno de la última preparación (o descarte de la preparada) que se ha pedido. Una preparación
/// cancelada puede seguir viva un rato —una descarga atascada no se puede interrumpir desde
/// fuera— mientras empieza la siguiente: solo la del último turno toca el ejecutable preparado y
/// su json, y siempre con `STAGE_FILES` cogido, para que la vieja no borre lo que deja la nueva.
static STAGE_TURN: AtomicU64 = AtomicU64::new(0);
static STAGE_FILES: Mutex<()> = Mutex::new(());

fn next_turn() -> u64 {
    STAGE_TURN.fetch_add(1, Ordering::SeqCst) + 1
}

fn is_latest(turn: u64) -> bool {
    STAGE_TURN.load(Ordering::SeqCst) == turn
}

/// Un hilo que se cayó con el cerrojo cogido no debe dejar la actualización bloqueada para siempre.
fn lock_files(files: &Mutex<()>) -> MutexGuard<'_, ()> {
    files.lock().unwrap_or_else(|e| e.into_inner())
}

/// Prepara la versión nueva en un hilo: comprueba que la app puede actualizarse desde su
/// carpeta, descarga el zip en `work_dir` (`<state_dir>/update`, fuera de carpetas sincronizadas
/// como OneDrive), lo verifica con el tamaño y el SHA-256 que publica GitHub, deja el ejecutable
/// junto al actual, lo arranca con `--self-test` y escribe `nanofy.update.json`. El progreso y el
/// final llegan por el bus (`Msg::UpdateStage`) con el turno que devuelve: la app descarta los de
/// una preparación anterior que aún estuvieran en camino. Con `cancel` activado se para en cuanto
/// puede, borra lo que llevaba y no manda nada más: quien cancela ya puso el estado que toca.
pub fn stage(ui: UiTx, info: UpdateInfo, work_dir: PathBuf, cancel: Arc<AtomicBool>) -> u64 {
    let turn = next_turn();
    let ui_err = ui.clone();
    let spawn = std::thread::Builder::new()
        .name("nanofy-update-stage".to_string())
        .spawn(move || {
            let mut report = |s: Stage| {
                if !cancel.load(Ordering::Relaxed) {
                    ui.send(Msg::UpdateStage { turn, stage: s });
                }
            };
            let Some(exe) = target_exe() else {
                report(Stage::Failed { kind: FailKind::Disk, detail: "No se encuentra el ejecutable actual".to_string() });
                return;
            };
            let test = |staged: &Path, version: &str| self_test(staged, version, SELF_TEST_LIMIT);
            let latest = move || is_latest(turn);
            let env = StageEnv { exe, work_dir, attempt: turn, files: &STAGE_FILES, latest: &latest, self_test: &test };
            let end = match stage_blocking(&env, &info, &cancel, &mut report) {
                Ok(staged) => {
                    // Cancelada justo al terminar (tras la última comprobación, con el json ya
                    // escrito): lo preparado se instalaría al abrir Nanofy aunque el usuario lo
                    // canceló o apagó «Actualizar automáticamente». Si ya se pidió otra, es suya.
                    if cancel.load(Ordering::SeqCst) {
                        let _files = lock_files(&STAGE_FILES);
                        if is_latest(turn) {
                            discard_staged(&env.exe);
                        }
                        log::info!("[update] preparación de {} cancelada al terminar", info.version);
                        return;
                    }
                    log::info!("[update] versión {} preparada y probada en {}", info.version, staged.display());
                    // Sin pasar por `report`: si se cancela ahora mismo, la app tiene que recibirlo
                    // igualmente para descartarlo (ver `App::on_update_stage`).
                    ui.send(Msg::UpdateStage { turn, stage: Stage::Ready { version: info.version.clone(), staged } });
                    return;
                }
                Err(StageError::Cancelled) => {
                    log::info!("[update] preparación de {} cancelada", info.version);
                    return;
                }
                Err(StageError::Move(dir, reason)) => {
                    log::warn!("[update] no se puede actualizar desde {} ({reason:?})", dir.display());
                    Stage::NeedsMove { dir, reason }
                }
                Err(StageError::Fail(kind, detail)) => {
                    log::warn!("[update] preparación fallida ({kind:?}): {detail}");
                    Stage::Failed { kind, detail }
                }
            };
            report(end);
        });
    if let Err(e) = spawn {
        log::warn!("[update] no se pudo lanzar la instalación: {e}");
        // Sin esto el aviso se quedaría en «Descargando…» esperando a un hilo que no existe.
        ui_err.send(Msg::UpdateStage { turn, stage: Stage::Failed { kind: FailKind::Disk, detail: format!("No se pudo empezar la descarga ({e})") } });
    }
    turn
}

/// Descarta la versión preparada (ha salido otra más nueva y no se va a preparar sola, o el
/// usuario ya no quiere que se instale sin preguntar), para que no se instale al abrir Nanofy.
/// En un hilo y con su turno, como una preparación: si después se pide otra, este ya no borra
/// nada (la nueva empieza quitando el json y sobrescribe el ejecutable).
pub fn discard_staged_async() {
    let Some(target) = target_exe() else {
        return;
    };
    let turn = next_turn();
    let spawn = std::thread::Builder::new().name("nanofy-update-discard".to_string()).spawn(move || {
        let _files = lock_files(&STAGE_FILES);
        if is_latest(turn) {
            discard_staged(&target);
            log::info!("[update] versión preparada descartada");
        }
    });
    if let Err(e) = spawn {
        log::warn!("[update] no se pudo descartar la versión preparada: {e}");
    }
}

/// Descargas a medias de un intento de `stage` (el zip y el ejecutable extraído). Llevan el pid
/// y el turno: ni un intento cancelado que siga atascado ni otra ventana de Nanofy (comparten la
/// carpeta de estado) escriben en el mismo archivo.
fn attempt_parts(work_dir: &Path, version: &str, attempt: u64) -> (PathBuf, PathBuf) {
    let tag = format!("{}-{attempt}", std::process::id());
    (work_dir.join(format!("Nanofy-{version}.{tag}.zip.part")), work_dir.join(format!("{}.{tag}.part", binary_name_in_zip())))
}

/// «Cancelar»: borra ya las descargas a medias del intento `attempt` (el turno de `stage`),
/// aunque su hilo siga esperando a la red. Rust abre los archivos dejando borrarlos: si el hilo
/// aún lo tiene abierto, desaparece al cerrarlo.
pub fn remove_attempt_downloads(work_dir: &Path, version: &str, attempt: u64) {
    let (zip_part, exe_part) = attempt_parts(work_dir, version, attempt);
    let _ = std::fs::remove_file(zip_part);
    let _ = std::fs::remove_file(exe_part);
}

/// Una descarga a medias sin tocar desde hace esto es de una ventana que se cerró a mitad (la que
/// está descargando la reescribe a cada momento).
const STALE_PART: Duration = Duration::from_secs(3600);

/// Borra de `work_dir` las descargas a medias abandonadas (más antiguas que `older_than`). Las
/// recientes pueden ser de otra ventana de Nanofy que está descargando ahora mismo.
fn remove_stale_downloads(work_dir: &Path, older_than: Duration) {
    for entry in std::fs::read_dir(work_dir).into_iter().flatten().flatten() {
        if !entry.file_name().to_string_lossy().ends_with(".part") {
            continue;
        }
        let age = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        if age.is_some_and(|a| a >= older_than) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Notas de la versión preparada, en la carpeta de trabajo (`<state_dir>/update`). La ventana que
/// se abre tras instalarla no las tiene (para ella ya no hay versión nueva) y las enseña en el
/// aviso de después de actualizar sin volver a pedirlas a GitHub.
#[derive(serde::Serialize, serde::Deserialize)]
struct SavedNotes {
    version: String,
    page_url: String,
    notes: String,
}

fn notes_path(work_dir: &Path) -> PathBuf {
    work_dir.join("notes.json")
}

/// Guarda las notas de `info` (al quedar lista para instalarse). Una sola a la vez: la última
/// preparada es la que se instala.
pub fn save_notes(work_dir: &Path, info: &UpdateInfo) {
    let saved = SavedNotes { version: info.version.clone(), page_url: info.page_url.clone(), notes: info.notes.clone() };
    if !write_json(&notes_path(work_dir), &saved) {
        log::debug!("[update] no se pudieron guardar las notas de la {}", info.version);
    }
}

/// Las notas guardadas, si son de `version` (la que corre ahora); las de otra no dicen nada de esta.
pub fn load_notes(work_dir: &Path, version: &str) -> Option<UpdateInfo> {
    let saved: SavedNotes = read_json(&notes_path(work_dir))?;
    (saved.version == version).then(|| UpdateInfo { version: saved.version, page_url: saved.page_url, notes: saved.notes, asset: None })
}

/// Lo que necesita `stage_blocking` además de la release (las pruebas ponen sus carpetas y su
/// autoprueba).
struct StageEnv<'a> {
    /// Ejecutable que se va a sustituir.
    exe: PathBuf,
    /// Carpeta de trabajo para el zip y la extracción.
    work_dir: PathBuf,
    /// Número de este intento, en el nombre de sus descargas: uno cancelado que siga atascado
    /// no escribe en el mismo archivo que el siguiente.
    attempt: u64,
    /// Cerrojo de `nanofy.update.exe` y su json (ver `STAGE_FILES`).
    files: &'a Mutex<()>,
    /// Si este intento sigue siendo el último pedido; si no, no toca esos archivos.
    latest: &'a dyn Fn() -> bool,
    /// Arranca el ejecutable preparado y dice si responde con la versión esperada.
    self_test: &'a dyn Fn(&Path, &str) -> Result<(), String>,
}

/// Cómo terminó la preparación cuando no sale bien.
#[derive(Debug)]
enum StageError {
    Fail(FailKind, String),
    Move(PathBuf, MoveReason),
    Cancelled,
}

fn fail(kind: FailKind, detail: impl Into<String>) -> StageError {
    StageError::Fail(kind, detail.into())
}

/// Error al escribir en el disco. El disco lleno se dice tal cual: es lo único que el usuario
/// puede arreglar.
fn disk(what: &str, e: std::io::Error) -> StageError {
    if e.kind() == std::io::ErrorKind::StorageFull {
        fail(FailKind::Disk, DISK_FULL_TEXT)
    } else {
        fail(FailKind::Disk, format!("{what}: {e}"))
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), StageError> {
    if cancel.load(Ordering::Relaxed) {
        Err(StageError::Cancelled)
    } else {
        Ok(())
    }
}

/// La preparación entera; devuelve la ruta del ejecutable preparado. Un intento que no termina
/// bien (también si se cancela) no deja nada: ni la descarga ni el ejecutable a medias.
fn stage_blocking(env: &StageEnv, info: &UpdateInfo, cancel: &AtomicBool, report: &mut dyn FnMut(Stage)) -> Result<PathBuf, StageError> {
    let Some(asset) = info.asset.as_ref() else {
        return Err(fail(FailKind::NoAsset, NO_ASSET_TEXT));
    };
    if let Some(reason) = location_problem(&env.exe) {
        let dir = env.exe.parent().map(Path::to_path_buf).unwrap_or_default();
        return Err(StageError::Move(dir, reason));
    }
    let staged = staged_exe_path(&env.exe);
    let meta = staged_meta_path(&env.exe);
    let (zip_part, exe_part) = attempt_parts(&env.work_dir, &info.version, env.attempt);
    {
        let _files = lock_files(env.files);
        if !(env.latest)() {
            return Err(StageError::Cancelled);
        }
        // El json va lo primero: es la marca de «listo», y una versión preparada antes (o un
        // intento a medias) no debe seguir dada por buena mientras su ejecutable se sobrescribe.
        let _ = std::fs::remove_file(&meta);
        remove_stale_downloads(&env.work_dir, STALE_PART);
    }
    let result = stage_steps(env, info, asset, cancel, report, &zip_part, &exe_part, &staged, &meta);
    let _ = std::fs::remove_file(&zip_part);
    let _ = std::fs::remove_file(&exe_part);
    if result.is_err() {
        let _files = lock_files(env.files);
        // Si ya se pidió otra preparación, esos archivos son suyos: no se tocan.
        if (env.latest)() {
            let _ = std::fs::remove_file(&meta);
            remove_retrying(&staged);
        }
    }
    result.map(|()| staged)
}

#[allow(clippy::too_many_arguments)]
fn stage_steps(
    env: &StageEnv,
    info: &UpdateInfo,
    asset: &Asset,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(Stage),
    zip_part: &Path,
    exe_part: &Path,
    staged: &Path,
    meta: &Path,
) -> Result<(), StageError> {
    std::fs::create_dir_all(&env.work_dir).map_err(|e| disk("No se pudo crear la carpeta de la descarga", e))?;
    download(asset, zip_part, cancel, report)?;
    report(Stage::Preparing);
    let (exe_size, exe_sha256) = extract(zip_part, exe_part)?;
    let _ = std::fs::remove_file(zip_part);
    // Desde aquí se tocan los archivos junto al ejecutable: de uno en uno, y solo si este sigue
    // siendo el último intento pedido (uno que se canceló y se atascó llega aquí tarde).
    let _files = lock_files(env.files);
    if !(env.latest)() {
        return Err(StageError::Cancelled);
    }
    check_cancel(cancel)?;
    place(exe_part, staged, exe_size)?;
    let _ = std::fs::remove_file(exe_part);
    check_cancel(cancel)?;
    (env.self_test)(staged, &info.version).map_err(|why| {
        let who = if cfg!(target_os = "windows") { "Windows o tu antivirus bloqueó" } else { "El sistema no dejó arrancar" };
        fail(FailKind::Blocked, format!("{who} la versión nueva ({why}). Sigues usando la {}.", current_version()))
    })?;
    check_cancel(cancel)?;
    write_meta(meta, &StagedMeta { version: info.version.clone(), target: env.exe.clone(), exe_size, exe_sha256: hex(&exe_sha256), swap_failures: 0 })
}

/// Agente para el zip: plazos por fase en vez de uno global, y el de recibir el cuerpo según el
/// tamaño (a 40 KB/s, entre 2 y 15 min), para que una conexión lenta no se corte a medias.
fn download_agent(asset: &Asset) -> ureq::Agent {
    let body_secs = match asset.size {
        0 => 900,
        size => (size / (40 * 1024)).clamp(120, 900),
    };
    let mut config = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(body_secs)))
        .http_status_as_error(false)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        );
    // Un servidor de pruebas en este equipo nunca va por el proxy del sistema.
    if is_local_url(&asset.url) {
        config = config.proxy(None);
    }
    ureq::Agent::new_with_config(config.build())
}

/// Descarga el zip en `zip_part` calculando su SHA-256 por el camino. Siempre desde cero: con el
/// plazo según el tamaño, 8 MB no compensan reanudar a medias.
fn download(asset: &Asset, zip_part: &Path, cancel: &AtomicBool, report: &mut dyn FnMut(Stage)) -> Result<(), StageError> {
    use sha2::Digest;
    use std::io::{Read, Write};
    let mut resp = download_agent(asset)
        .get(&asset.url)
        .header("User-Agent", &format!("Nanofy/{} (+https://github.com/{REPO})", current_version()))
        .call()
        .map_err(|e| fail(FailKind::Network, format!("Sin conexión con GitHub ({e})")))?;
    if !resp.status().is_success() {
        return Err(fail(FailKind::Network, format!("GitHub respondió HTTP {}", resp.status().as_u16())));
    }
    // El tamaño que publica GitHub manda: es el del archivo, mientras que Content-Length puede
    // faltar o contar los bytes comprimidos en tránsito.
    let total = (asset.size > 0).then_some(asset.size).or_else(|| {
        resp.headers()
            .get("Content-Length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
    });
    let limit = if asset.size > 0 { asset.size } else { MAX_DOWNLOAD };
    let mut reader = resp.body_mut().as_reader();
    let mut file = std::fs::File::create(zip_part).map_err(|e| disk("No se pudo crear la descarga", e))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let (mut done, mut reported) = (0u64, 0u64);
    let mut reported_at = Instant::now();
    report(Stage::Downloading { done: 0, total });
    loop {
        check_cancel(cancel)?;
        let n = reader.read(&mut buf).map_err(|e| fail(FailKind::Network, format!("Descarga interrumpida: {e}")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| disk("No se pudo guardar la descarga", e))?;
        hasher.update(&buf[..n]);
        done += n as u64;
        // Si ya pasa del tamaño que publica GitHub no es ese archivo: se corta aquí en vez de
        // seguir llenando el disco hasta que el servidor deje de mandar.
        if done > limit {
            return Err(fail(FailKind::Corrupt, format!("La descarga no coincide con la release (más de {limit} bytes)")));
        }
        if done - reported >= 256 * 1024 || reported_at.elapsed() >= Duration::from_millis(100) {
            reported = done;
            reported_at = Instant::now();
            report(Stage::Downloading { done, total });
        }
    }
    drop(file);
    if let Some(t) = total.filter(|t| done < *t) {
        return Err(fail(FailKind::Network, format!("Descarga incompleta ({done} de {t} bytes)")));
    }
    if let Some(want) = asset.sha256 {
        if hasher.finalize().as_slice() != want {
            return Err(fail(FailKind::Corrupt, "La descarga llegó dañada (su SHA-256 no es el que publica la release) y se descartó."));
        }
    }
    Ok(())
}

/// La entrada del zip es el ejecutable: su último componente es `want`, partiendo por «/» y por
/// «\» (hay compresores de Windows que usan esta) y sin mirar mayúsculas.
fn is_binary_entry(name: &str, want: &str) -> bool {
    name.rsplit(['/', '\\']).next().is_some_and(|last| last.eq_ignore_ascii_case(want))
}

/// Saca el ejecutable del zip a `out_path` y comprueba que lo parece (cabecera y tamaño).
/// Devuelve su tamaño y su SHA-256 para `nanofy.update.json`.
fn extract(zip_part: &Path, out_path: &Path) -> Result<(u64, [u8; 32]), StageError> {
    use sha2::Digest;
    use std::io::{Read, Write};
    let f = std::fs::File::open(zip_part).map_err(|e| disk("No se pudo abrir la descarga", e))?;
    let mut archive = zip::ZipArchive::new(f).map_err(|e| fail(FailKind::Corrupt, format!("Zip no válido: {e}")))?;
    let want = binary_name_in_zip();
    let idx = (0..archive.len())
        .find(|&i| archive.name_for_index(i).is_some_and(|n| is_binary_entry(n, want)))
        .ok_or_else(|| fail(FailKind::Corrupt, format!("El zip no contiene {want}")))?;
    let mut entry = archive.by_index(idx).map_err(|e| fail(FailKind::Corrupt, format!("Zip no válido: {e}")))?;
    let mut out = std::fs::File::create(out_path).map_err(|e| disk("No se pudo extraer el ejecutable", e))?;
    let mut hasher = sha2::Sha256::new();
    let mut head: Vec<u8> = Vec::with_capacity(4);
    let mut size = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        // Un zip roto se nota al descomprimir (InvalidData, también si no cuadra su CRC); lo
        // demás es el disco.
        let n = entry.read(&mut buf).map_err(|e| {
            if e.kind() == std::io::ErrorKind::InvalidData {
                fail(FailKind::Corrupt, format!("No se pudo extraer el ejecutable: {e}"))
            } else {
                disk("No se pudo extraer el ejecutable", e)
            }
        })?;
        if n == 0 {
            break;
        }
        if head.len() < 4 {
            let take = (4 - head.len()).min(n);
            head.extend_from_slice(&buf[..take]);
        }
        out.write_all(&buf[..n]).map_err(|e| disk("No se pudo extraer el ejecutable", e))?;
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    drop(out);
    if size < MIN_EXE_SIZE || !head.starts_with(exe_magic()) {
        return Err(fail(FailKind::Corrupt, "El ejecutable descargado no parece válido"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(out_path, std::fs::Permissions::from_mode(0o755));
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&hasher.finalize());
    Ok((size, digest))
}

/// Copia el ejecutable comprobado junto al actual y lo fuerza al disco, de modo que tras un corte
/// de luz o está entero o no vale (sin el json no cuenta).
fn place(from: &Path, staged: &Path, size: u64) -> Result<(), StageError> {
    remove_retrying(staged);
    let what = format!("No se pudo dejar la versión nueva en {}", staged.parent().unwrap_or(staged).display());
    std::fs::copy(from, staged).map_err(|e| disk(&what, e))?;
    let f = std::fs::OpenOptions::new().write(true).open(staged).map_err(|e| disk(&what, e))?;
    f.sync_all().map_err(|e| disk(&what, e))?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len != size {
        return Err(fail(FailKind::Disk, format!("{what}: la copia quedó incompleta ({len} de {size} bytes)")));
    }
    Ok(())
}

/// Escribe `nanofy.update.json`; sin él, el ejecutable preparado no cuenta.
fn write_meta(path: &Path, meta: &StagedMeta) -> Result<(), StageError> {
    if write_json(path, meta) {
        Ok(())
    } else {
        Err(fail(FailKind::Disk, format!("No se pudo guardar {}", path.display())))
    }
}

/// Escribe `value` de una vez (archivo temporal y renombrado) y lo relee, porque `write_atomic`
/// no avisa si falla.
fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> bool {
    let Ok(text) = serde_json::to_string_pretty(value) else {
        return false;
    };
    crate::cache::write_atomic(path, &text);
    std::fs::read_to_string(path).is_ok_and(|t| t == text)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Borra `path` aunque un antivirus (o el proceso de la autoprueba, al terminar) lo retenga un
/// momento: unos pocos intentos, que van en el hilo de la actualización.
fn remove_retrying(path: &Path) {
    for attempt in 1..=8 {
        match std::fs::remove_file(path) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) if attempt == 8 => log::warn!("[update] no se pudo borrar {}: {e}", path.display()),
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// Renombra `from` a `to` con unos pocos reintentos en Windows: justo después de la autoprueba,
/// un antivirus o OneDrive (si la carpeta está sincronizada) pueden tener abierto el ejecutable
/// nuevo un momento y el renombrado da «acceso denegado» o «en uso por otro proceso» (32, 33).
/// En los demás sistemas un error así no se arregla esperando.
pub fn rename_retrying(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 1;
    loop {
        match std::fs::rename(from, to) {
            Err(e) if attempt < 8 && is_transient_lock(&e) => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(250));
            }
            other => return other,
        }
    }
}

fn is_transient_lock(e: &std::io::Error) -> bool {
    cfg!(windows) && (e.kind() == std::io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(32 | 33)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Arranca `exe --self-test` sin ventana y exige que termine bien y responda `nanofy {version}`
/// antes de `limit`. Así se sabe, sin tocar el ejecutable en uso, que Windows, Smart App Control
/// o el antivirus dejan abrir la versión nueva: cada versión sin firmar es un archivo nuevo para
/// ellos. `Err` lleva el motivo para el aviso.
fn self_test(exe: &Path, version: &str, limit: Duration) -> Result<(), String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    if !exe.exists() {
        // Un antivirus que lo pone en cuarentena lo hace desaparecer sin más.
        return Err("el archivo nuevo desapareció".to_string());
    }
    let mut cmd = Command::new(exe);
    cmd.arg("--self-test")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_remove("NANOFY_VERSION");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: ni un parpadeo de consola si la versión nueva fuese de consola.
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = spawn_retrying(&mut cmd).map_err(|e| spawn_error(&e))?;
    let stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    // La salida se lee en otro hilo: si algo heredase la tubería y no la cerrase, aquí no se
    // espera por ella más que un momento.
    let _ = std::thread::Builder::new().name("nanofy-update-selftest".to_string()).spawn(move || {
        let mut out = Vec::new();
        if let Some(s) = stdout {
            let _ = s.take(4096).read_to_end(&mut out);
        }
        let _ = tx.send(out);
    });
    let t0 = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if t0.elapsed() < limit => std::thread::sleep(Duration::from_millis(20)),
            other => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match other {
                    Err(e) => format!("no se pudo esperar a la autoprueba: {e}"),
                    _ => format!("la autoprueba no respondió en {} s", limit.as_secs()),
                });
            }
        }
    };
    let out = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    let out = String::from_utf8_lossy(&out);
    if !status.success() {
        return Err(format!("la autoprueba terminó con error ({status})"));
    }
    let want = format!("nanofy {version}");
    if out.trim() != want {
        return Err(format!("la autoprueba respondió «{}» en vez de «{want}»", out.trim()));
    }
    Ok(())
}

/// En Linux, un ejecutable recién escrito puede dar «Text file busy» un instante si otro hilo
/// lanza un proceso a la vez (el hijo hereda el descriptor hasta su exec): se reintenta.
fn spawn_retrying(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    let mut tries = 0;
    loop {
        match cmd.spawn() {
            Err(e) if cfg!(unix) && e.raw_os_error() == Some(26) && tries < 5 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            other => return other,
        }
    }
}

/// Motivo corto de un arranque rechazado para el aviso, con los códigos que dan los bloqueos de
/// Windows. El texto completo del sistema (largo y en el idioma de Windows) va al registro.
fn spawn_error(e: &std::io::Error) -> String {
    log::warn!("[update] la versión nueva no arrancó: {e}");
    match e.raw_os_error() {
        Some(225) if cfg!(windows) => "el antivirus la tomó por una amenaza, código 225".to_string(),
        Some(1260) if cfg!(windows) => "una directiva del sistema impide abrirla, código 1260".to_string(),
        Some(4551) if cfg!(windows) => "Control inteligente de aplicaciones no deja abrirla, código 4551".to_string(),
        Some(code) => format!("no arrancó, código {code}"),
        None => format!("no arrancó: {e}"),
    }
}

/// Si la app no puede actualizarse sola desde donde está `exe`. Toca el disco (crea y borra un
/// archivo de prueba): va en el hilo de la actualización, nunca en el de la interfaz.
pub fn location_problem(exe: &Path) -> Option<MoveReason> {
    if running_from_zip(exe) {
        return Some(MoveReason::RunningFromZip);
    }
    let probe = exe.with_extension("update.probe");
    match std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&probe) {
        Ok(f) => {
            drop(f);
            let _ = std::fs::remove_file(&probe);
            None
        }
        Err(e) if is_read_only(&e) => Some(MoveReason::ReadOnly),
        Err(e) => {
            // Otro error no prueba que la carpeta sea de solo lectura: se intenta igual y, si
            // falla, el aviso dirá el motivo real.
            log::info!("[update] no se pudo probar la carpeta de {}: {e}", exe.display());
            None
        }
    }
}

fn is_read_only(e: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    // 19 = ERROR_WRITE_PROTECT (unidad o recurso compartido de solo lectura).
    matches!(e.kind(), ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem) || (cfg!(windows) && e.raw_os_error() == Some(19))
}

/// El ejecutable está en la carpeta temporal en la que un compresor abre lo que hay dentro de
/// un zip: `Temp1_…` (explorador de Windows; `Temp2_…` si se repite), `Rar$EX…` (WinRAR) o
/// `7zO…` (7-Zip). Solo esas: una carpeta cualquiera dentro de %TEMP% vale, y desde ahí corren
/// las pruebas de QA.
fn running_from_zip(exe: &Path) -> bool {
    fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
        let head = s.get(..prefix.len())?;
        head.eq_ignore_ascii_case(prefix).then(|| &s[prefix.len()..])
    }
    let path = exe.to_string_lossy();
    path.split(['/', '\\']).any(|c| {
        let explorer = strip_prefix_ci(c, "Temp").and_then(|r| r.split_once('_')).is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        let winrar = strip_prefix_ci(c, "Rar$EX").is_some();
        let sevenzip = strip_prefix_ci(c, "7zO").is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_hexdigit()));
        explorer || winrar || sevenzip
    })
}

// ------------------------------------------------------------ sustitución, arranque y salud
//
// Windows no deja sobrescribir ni borrar un ejecutable en uso, pero sí renombrarlo: el actual se
// aparta a `nanofy.old.exe` y el preparado ocupa su sitio. Nunca dentro de `on_exit`: el vigilante
// de salida de 3 s (shell.rs) podría matar el proceso entre los dos renombrados y dejar al usuario
// sin nanofy.exe. Solo con la ventana aún abierta (en un hilo) o al arrancar, antes de abrirla.

/// Arranques de la versión instalada sin llegar a darse por buena tras los que se vuelve a la
/// anterior (el que lo decide cuenta: tras dos que no llegaron, el tercero ya vuelve).
const ROLLBACK_BOOTS: u32 = 3;
/// Tiempo desde el primer fotograma tras el que la versión en uso se da por buena.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(20);
/// Arranques en los que se intenta instalar la versión preparada antes de descartarla.
const MAX_SWAP_FAILURES: u8 = 3;
/// Un cerrojo con más de esto es de un proceso que murió a medias: una sustitución dura segundos.
const LOCK_STALE: Duration = Duration::from_secs(120);
/// Rondas de renombrado, cada 250 ms (unos 2 s en total): lo que un antivirus u OneDrive suelen
/// retener un archivo recién escrito.
const SWAP_ROUNDS: u32 = 8;

/// Esta ejecución es la de una versión recién instalada que aún no se ha dado por buena.
static PROBATION: AtomicBool = AtomicBool::new(false);
/// Versión que no llegaba a arrancar y se devolvió, leída al arrancar.
static BAD_VERSION: OnceLock<Option<String>> = OnceLock::new();

/// Puerta de las sustituciones: `on_exit` la cierra lo primero. Cerrarla espera a que termine el
/// renombrado en curso (milisegundos) y desde ahí no empieza ninguno, así que el proceso no puede
/// terminar entre «apartar el actual» y «poner el nuevo».
static SWAP_GATE: Mutex<bool> = Mutex::new(false);

pub fn close_swap_gate() {
    *SWAP_GATE.lock().unwrap_or_else(|e| e.into_inner()) = true;
}

/// `nanofy.update.applied.json`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct AppliedMeta {
    /// Versión de la que se vino, la que quedó en `old`.
    from: String,
    /// Versión instalada.
    to: String,
    /// Ejecutable anterior, apartado.
    old: PathBuf,
    /// Arranques de la instalada que aún no han llegado a darla por buena.
    #[serde(default)]
    boots: u32,
    #[serde(default)]
    healthy: bool,
}

/// `nanofy.update.bad.json`. La consulta automática no vuelve a ofrecer esa versión; a mano, sí.
#[derive(serde::Serialize, serde::Deserialize)]
struct BadMeta {
    version: String,
}

/// La versión `version` ya se instaló una vez aquí y no llegaba a arrancar.
pub fn is_bad_version(version: &str) -> bool {
    BAD_VERSION.get().and_then(|v| v.as_deref()) == Some(version)
}

/// Cerrojo entre procesos mientras se renombra el ejecutable: dos ventanas de Nanofy (o la que se
/// cierra y la que arranca) no deben sustituirlo a la vez. Se suelta al soltar el valor.
struct SwapLock(PathBuf);

impl SwapLock {
    fn acquire(target: &Path) -> Result<Self, String> {
        let path = lock_path(target);
        let busy = || "otra ventana de Nanofy está instalando la actualización".to_string();
        for first in [true, false] {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = write!(f, "{}", std::process::id());
                    return Ok(Self(path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && first && lock_is_stale(&path) => {
                    log::warn!("[update] cerrojo abandonado en {}; se quita", path.display());
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(busy()),
                Err(e) => return Err(format!("no se pudo crear {}: {e}", path.display())),
            }
        }
        Err(busy())
    }
}

impl Drop for SwapLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn lock_is_stale(path: &Path) -> bool {
    match std::fs::metadata(path).and_then(|m| m.modified()) {
        // Con fecha en el futuro solo si es por mucho: un reloj algo adelantado (una carpeta de
        // red) no debe dar por abandonado el cerrojo de una sustitución en marcha.
        Ok(t) => match t.elapsed() {
            Ok(age) => age >= LOCK_STALE,
            Err(ahead) => ahead.duration() >= LOCK_STALE,
        },
        Err(_) => false,
    }
}

/// Pone `incoming` en el sitio de `target` y deja el que había en `aside`. Si `incoming` no se
/// puede colocar, el de antes vuelve a su sitio en el acto, antes de esperar para reintentar: el
/// hueco sin ejecutable dura lo que dos renombrados, nunca una espera. `on_swapped` va dentro de
/// la puerta, para que el registro de lo hecho no quede a medias si la app se cierra.
fn swap_in(target: &Path, incoming: &Path, aside: &Path, gate: &Mutex<bool>, on_swapped: &mut dyn FnMut()) -> Result<(), String> {
    let mut round = 1;
    loop {
        let busy = {
            let closed = gate.lock().unwrap_or_else(|e| e.into_inner());
            if *closed {
                return Err("Nanofy se está cerrando".to_string());
            }
            match std::fs::rename(target, aside) {
                // Aún no se ha movido nada: se puede esperar sin prisa.
                Err(e) if round < SWAP_ROUNDS && is_transient_lock(&e) => e,
                Err(e) => return Err(format!("no se pudo apartar el ejecutable actual: {e}")),
                Ok(()) => match std::fs::rename(incoming, target) {
                    Ok(()) => {
                        on_swapped();
                        return Ok(());
                    }
                    Err(e) => {
                        put_back(aside, target)?;
                        if !(round < SWAP_ROUNDS && is_transient_lock(&e)) {
                            return Err(format!("no se pudo colocar el ejecutable nuevo: {e}"));
                        }
                        e
                    }
                },
            }
        };
        log::debug!("[update] sustitución ocupada ({busy}); intento {round} de {SWAP_ROUNDS}");
        round += 1;
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Devuelve el ejecutable apartado a su sitio: sin él, el acceso directo del usuario no abre
/// nada. Si ni renombrando se puede, se copia (y el apartado queda como copia de más).
fn put_back(aside: &Path, target: &Path) -> Result<(), String> {
    let Err(e) = rename_retrying(aside, target) else {
        return Ok(());
    };
    log::error!("[update] no se pudo devolver {} a su sitio ({e}); se copia", target.display());
    match std::fs::copy(aside, target) {
        Ok(_) => Ok(()),
        Err(c) => {
            log::error!("[update] tampoco copiándolo: {c}");
            Err(format!("no se pudo devolver el ejecutable actual a su sitio ({c}); está en {}", aside.display()))
        }
    }
}

/// Nombre libre para apartar un ejecutable: `preferred`, borrando el que hubiera, o si ese sigue
/// en uso (otra ventana abierta con la versión de antes) el mismo con el pid de este proceso.
fn pick_aside(preferred: &Path) -> Result<PathBuf, String> {
    let alt = with_pid(preferred);
    for p in [preferred, alt.as_path()] {
        match std::fs::remove_file(p) {
            Ok(()) => return Ok(p.to_path_buf()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(p.to_path_buf()),
            Err(e) => log::info!("[update] {} sigue en uso ({e})", p.display()),
        }
    }
    Err(format!("{} está en uso", preferred.display()))
}

/// `nanofy.old.exe` → `nanofy.old-<pid>.exe` (en Linux, `nanofy.old` → `nanofy.old-<pid>`).
fn with_pid(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let pid = std::process::id();
    let name = match name.strip_suffix(".exe") {
        Some(stem) => format!("{stem}-{pid}.exe"),
        None => format!("{name}-{pid}"),
    };
    path.with_file_name(name)
}

/// El mismo archivo aunque una ruta venga con otras mayúsculas o por otro camino (cada proceso la
/// lee de cómo lo abrieron: acceso directo, explorador…).
fn same_path(a: &Path, b: &Path) -> bool {
    let eq = |x: &Path, y: &Path| {
        if cfg!(windows) {
            x.to_string_lossy().eq_ignore_ascii_case(&y.to_string_lossy())
        } else {
            x == y
        }
    };
    if eq(a, b) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => eq(&x, &y),
        _ => false,
    }
}

fn file_sha256(path: &Path) -> std::io::Result<[u8; 32]> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&hasher.finalize());
    Ok(out)
}

/// Borra la versión preparada: primero su json (la marca de «lista»), luego el ejecutable.
fn discard_staged(target: &Path) {
    let _ = std::fs::remove_file(staged_meta_path(target));
    remove_retrying(&staged_exe_path(target));
}

/// Comprueba que la versión preparada es justo la que pasó la autoprueba y que vale para
/// `target`. `Err` lleva el motivo y si hay que descartarla (`true`) o solo esperar a otro
/// intento (no se pudo leer: un antivirus la tiene abierta, por ejemplo).
fn check_staged(meta: &StagedMeta, target: &Path, staged: &Path, current: &str) -> Result<(), (String, bool)> {
    if !same_path(&meta.target, target) {
        return Err((format!("la versión preparada es para {}", meta.target.display()), true));
    }
    match (parse_version(&meta.version), parse_version(current)) {
        (Some(new), Some(cur)) if new > cur => {}
        _ => return Err((format!("la versión preparada ({}) no es más nueva que la {current}", meta.version), true)),
    }
    let len = match std::fs::metadata(staged) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(("la versión preparada ya no está; quizá la retiró el antivirus".to_string(), true));
        }
        Err(e) => return Err((format!("no se pudo leer la versión preparada: {e}"), false)),
    };
    if len != meta.exe_size {
        return Err((format!("la versión preparada está incompleta ({len} de {} bytes)", meta.exe_size), true));
    }
    match file_sha256(staged) {
        Ok(d) if hex(&d).eq_ignore_ascii_case(&meta.exe_sha256) => Ok(()),
        Ok(_) => Err(("la versión preparada cambió después de probarla".to_string(), true)),
        Err(e) => Err((format!("no se pudo leer la versión preparada: {e}"), false)),
    }
}

/// Instala la versión preparada (`nanofy.update.exe`, descrita por `meta`) en el sitio de
/// `target`: el actual queda como `nanofy.old.exe` y la nueva, a prueba. Si la preparada no vale
/// (falta, no es la que se probó, no es más nueva o es para otro ejecutable) se borra; si lo que
/// falla es la sustitución, se conserva para el siguiente intento. `target` debe tomarse antes de
/// renombrar nada (`target_exe`). Compara con `current_version()`: con `NANOFY_VERSION` fingida,
/// las pruebas instalan la misma compilación.
pub fn apply_staged(meta: &StagedMeta, target: &Path) -> Result<(), String> {
    apply_staged_in(meta, target, &current_version(), &SWAP_GATE)
}

fn apply_staged_in(meta: &StagedMeta, target: &Path, current: &str, gate: &Mutex<bool>) -> Result<(), String> {
    let staged = staged_exe_path(target);
    if let Err((why, discard)) = check_staged(meta, target, &staged, current) {
        if discard {
            discard_staged(target);
        }
        return Err(why);
    }
    let _lock = SwapLock::acquire(target)?;
    let aside = pick_aside(&old_exe_path(target))?;
    let applied = AppliedMeta { from: current.to_string(), to: meta.version.clone(), old: aside.clone(), boots: 0, healthy: false };
    let applied_path = applied_path(target);
    swap_in(target, &staged, &aside, gate, &mut || {
        if !write_json(&applied_path, &applied) {
            log::warn!("[update] no se pudo guardar {}: si la versión nueva no arranca, no se volverá sola a la anterior", applied_path.display());
        }
    })?;
    let _ = std::fs::remove_file(staged_meta_path(target));
    log::info!("[update] instalada la {} en {}; la {current} queda en {}", meta.version, target.display(), aside.display());
    Ok(())
}

/// «Reiniciar»: instala la versión preparada junto a `target`. Va en un hilo, con la ventana aún
/// abierta; al terminar, la app se cierra y abre la nueva.
pub fn apply_pending(target: &Path) -> Result<(), String> {
    let meta: StagedMeta = read_json(&staged_meta_path(target)).ok_or_else(|| "no hay ninguna versión preparada".to_string())?;
    apply_staged(&meta, target)
}

/// La versión `version` ya está preparada y probada junto a `exe`, entera y sin tocar: quedó de
/// una sesión anterior cuya instalación al abrir no pudo sustituir el ejecutable. No hace falta
/// descargarla otra vez, y prepararla de nuevo pondría a cero sus intentos (`swap_failures`): cada
/// arranque volvería a descargarla y a intentarlo sin llegar nunca a descartarla. No borra nada.
fn already_staged(exe: &Path, version: &str) -> Option<PathBuf> {
    let meta: StagedMeta = read_json(&staged_meta_path(exe))?;
    if meta.version != version {
        return None;
    }
    let staged = staged_exe_path(exe);
    check_staged(&meta, exe, &staged, &current_version()).ok()?;
    Some(staged)
}

/// Texto del aviso cuando falla `apply_pending`: si la versión preparada sigue ahí, se instalará
/// al abrir Nanofy la próxima vez.
pub fn swap_failure_text(target: &Path, why: &str) -> String {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| binary_name_in_zip().to_string());
    if staged_meta_path(target).exists() {
        format!("No se pudo sustituir {name} ({why}). Se intentará de nuevo al abrir Nanofy.")
    } else {
        format!("No se pudo instalar la versión nueva ({why}).")
    }
}

/// Argumentos de paso de una actualización: los lee el proceso nuevo y no deben arrastrarse (ni
/// acumularse) de un reinicio al siguiente.
pub fn strip_transient_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--updated-from" | "--wait-pid" | "--update-failed" => {
                // Su valor va detrás; si faltase, no se come el argumento siguiente.
                if it.peek().is_some_and(|v| !v.starts_with("--")) {
                    it.next();
                }
            }
            "--resume-playing" => {}
            _ => out.push(a.clone()),
        }
    }
    out
}

/// Argumentos con los que se abre la versión nueva tras «Reiniciar»: los de este proceso (p. ej.
/// `--control`, para que arranque igual), pero con la página en la que está ahora el usuario
/// (`page`) en vez de la de arranque. El `--page` original no se repite: podía ser una prueba
/// (`loadctx:`, `stall`) que pisaría la música que se retoma. `spawn_target` quita además los
/// argumentos de paso de una actualización anterior.
pub fn restart_args(args: &[String], page: Option<&str>) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len() + 2);
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if a == "--page" {
            if it.peek().is_some_and(|v| !v.starts_with("--")) {
                it.next();
            }
            continue;
        }
        out.push(a.clone());
    }
    if let Some(p) = page {
        out.push("--page".to_string());
        out.push(p.to_string());
    }
    out
}

/// Abre `target` con los mismos argumentos (sin los de paso de una actualización anterior), más
/// `extra` y `--wait-pid` de este proceso, para que espere a que este termine. Sin
/// `NANOFY_VERSION`: la versión fingida para probar ya cumplió y la nueva debe ser ella misma.
pub fn spawn_target(target: &Path, args: &[String], extra: &[String]) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(target);
    cmd.args(strip_transient_args(args))
        .args(extra)
        .arg("--wait-pid")
        .arg(std::process::id().to_string())
        .env_remove("NANOFY_VERSION");
    let child = spawn_retrying(&mut cmd)?;
    log::info!("[update] abierto {} (pid {})", target.display(), child.id());
    Ok(())
}

fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

/// Espera, como mucho `limit`, a que termine el proceso `pid` (la ventana que se cerró para
/// actualizar): así su despedida a Spotify no cae encima del registro de la nueva y no hay dos
/// Nanofy a la vez. Si ya terminó, no espera nada.
fn wait_for_exit(pid: u32, limit: Duration) {
    if pid == 0 || pid == std::process::id() {
        return;
    }
    let t0 = Instant::now();
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
        // Sin poder abrirlo es que ya no existe.
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            if WaitForSingleObject(h, limit.as_millis() as u32) != WAIT_OBJECT_0 {
                log::warn!("[update] la ventana anterior ({pid}) sigue abierta tras {} s", limit.as_secs());
            }
            let _ = CloseHandle(h);
        }
    }
    #[cfg(target_os = "linux")]
    {
        // Un proceso terminado sigue en /proc como zombi hasta que lo recoge su padre.
        let alive = || {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|s| s.rsplit_once(')').map(|(_, rest)| rest.trim_start().chars().next()))
                .is_some_and(|state| !matches!(state, Some('Z' | 'X')))
        };
        while alive() && t0.elapsed() < limit {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    // En macOS la app no se sustituye sola (`can_self_install`): nadie la abre con --wait-pid.
    #[cfg(not(any(windows, target_os = "linux")))]
    let _ = limit;
    log::info!("[update] esperada la ventana anterior ({pid}): {} ms", t0.elapsed().as_millis());
}

#[derive(Debug, PartialEq)]
enum Health {
    /// Nada a prueba.
    Normal,
    /// Esta es la versión recién instalada y aún no se ha dado por buena.
    Probation,
    /// No llegaba a arrancar: ya se volvió a la anterior, que hay que abrir.
    RolledBack { from: String, to: String },
}

/// Al arrancar: cuenta el arranque de la versión a prueba y, si ya lleva `ROLLBACK_BOOTS` sin
/// darse por buena, vuelve a la anterior. `this_version` es la versión compilada (la que responde
/// la autoprueba y queda en `to`), no `current_version()`: aquí importa qué binario corre.
fn check_health(target: &Path, this_version: &str, gate: &Mutex<bool>) -> Health {
    let path = applied_path(target);
    let Some(mut applied) = read_json::<AppliedMeta>(&path) else {
        return Health::Normal;
    };
    if applied.healthy {
        return Health::Normal;
    }
    if applied.to != this_version {
        // Alguien cambió el ejecutable a mano: el registro ya no describe lo que corre.
        log::info!("[update] {} es de la {} y esta es la {this_version}: se descarta", path.display(), applied.to);
        let _ = std::fs::remove_file(&path);
        return Health::Normal;
    }
    applied.boots += 1;
    if applied.boots < ROLLBACK_BOOTS {
        log::info!("[update] la {} está a prueba (arranque {} de {ROLLBACK_BOOTS})", applied.to, applied.boots);
        write_json(&path, &applied);
        return Health::Probation;
    }
    if !applied.old.exists() {
        log::warn!("[update] la {} no llega a arrancar, pero ya no está la {} para volver a ella", applied.to, applied.from);
        let _ = std::fs::remove_file(&path);
        return Health::Normal;
    }
    log::warn!("[update] la {} lleva {} arranques sin llegar a funcionar: se vuelve a la {}", applied.to, applied.boots, applied.from);
    match roll_back(target, &applied, gate) {
        Ok(()) => Health::RolledBack { from: applied.from, to: applied.to },
        Err(e) => {
            // Se sigue contando: el próximo arranque lo vuelve a intentar.
            log::error!("[update] no se pudo volver a la {}: {e}", applied.from);
            write_json(&path, &applied);
            Health::Probation
        }
    }
}

/// Vuelve a la versión anterior: la que no arranca pasa a `nanofy.bad.exe` y la anterior a su
/// sitio. Se apunta en `nanofy.update.bad.json` para no volver a instalarla sola.
fn roll_back(target: &Path, applied: &AppliedMeta, gate: &Mutex<bool>) -> Result<(), String> {
    let _lock = SwapLock::acquire(target)?;
    let aside = pick_aside(&bad_exe_path(target))?;
    let (bad_path, applied_path) = (bad_meta_path(target), applied_path(target));
    swap_in(target, &applied.old, &aside, gate, &mut || {
        if !write_json(&bad_path, &BadMeta { version: applied.to.clone() }) {
            log::warn!("[update] no se pudo guardar {}", bad_path.display());
        }
        // Ya no hay nada a prueba: lo que queda en su sitio es la de antes.
        let _ = std::fs::remove_file(&applied_path);
    })
}

/// Qué hacer tras `on_launch`.
pub enum Launch {
    /// Seguir arrancando. `notice`: la versión nueva no arrancaba y se sigue con esta.
    Continue { notice: Option<String> },
    /// Ya se abrió la versión que toca: este proceso termina sin abrir ventana.
    Exit,
}

/// Lo primero al arrancar, tras el registro y antes de leer ajustes o abrir la ventana: espera a
/// la ventana que se cerró para actualizar (`--wait-pid`), vuelve a la versión anterior si esta
/// no llega a arrancar e instala la versión que quedó preparada.
pub fn on_launch() -> Launch {
    let Some(target) = target_exe() else {
        return Launch::Continue { notice: None };
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(pid) = arg_value(&args, "--wait-pid").and_then(|p| p.parse::<u32>().ok()) {
        wait_for_exit(pid, Duration::from_secs(5));
    }
    let health = check_health(&target, env!("CARGO_PKG_VERSION"), &SWAP_GATE);
    match &health {
        Health::Normal => {}
        Health::Probation => PROBATION.store(true, Ordering::Relaxed),
        Health::RolledBack { from, to } => {
            let msg = format!("La versión {to} no arrancaba; se volvió a la {from}");
            if let Err(e) = spawn_target(&target, &args, &["--update-failed".to_string(), msg]) {
                log::error!("[update] no se pudo abrir la {from}: {e}");
            }
            // Esta es la que no arranca: no se sigue con ella. El acceso directo ya abre la otra.
            return Launch::Exit;
        }
    }
    let launch = if install_at_launch_allowed(&args, &health) {
        apply_at_launch(&target, &args)
    } else {
        if health == Health::Probation && staged_meta_path(&target).exists() {
            log::info!("[update] versión a prueba: la preparada se instalará cuando esta se dé por buena");
        }
        Launch::Continue { notice: None }
    };
    // Después de instalar al arrancar: si la nueva no abrió y se volvió a esta, ya está en
    // nanofy.update.bad.json y su aviso no debe volver a saltar solo en esta misma sesión.
    let _ = BAD_VERSION.set(read_json::<BadMeta>(&bad_meta_path(&target)).map(|b| b.version));
    launch
}

/// Si al arrancar se puede instalar la versión que quedó preparada. Nunca recién llegada de una
/// actualización o de una vuelta atrás (no se encadena otra). Tampoco con esta versión a prueba
/// (la vez anterior se cerró de golpe antes de darse por buena): instalar otra apartaría esta a
/// nanofy.old.exe borrando la anterior, la única que se sabe buena, y si esta tampoco funciona ya
/// no habría a qué volver. La preparada se queda para cuando esta se dé por buena.
fn install_at_launch_allowed(args: &[String], health: &Health) -> bool {
    *health == Health::Normal && !args.iter().any(|a| a == "--updated-from" || a == "--update-failed")
}

/// La versión preparada que no llegó a instalarse (se cerró Nanofy sin reiniciar, se colgó o se
/// apagó el equipo) se instala ahora, sin ventana ni vigilante de salida, y se abre la nueva.
fn apply_at_launch(target: &Path, args: &[String]) -> Launch {
    let meta_path = staged_meta_path(target);
    if !meta_path.exists() {
        return Launch::Continue { notice: None };
    }
    let Some(mut meta) = read_json::<StagedMeta>(&meta_path) else {
        log::warn!("[update] {} ilegible: se descarta", meta_path.display());
        discard_staged(target);
        return Launch::Continue { notice: None };
    };
    let from = current_version();
    log::info!("[update] instalando al arrancar la {} preparada (desde la {from})", meta.version);
    if let Err(e) = apply_staged(&meta, target) {
        log::warn!("[update] no se pudo instalar al arrancar: {e}");
        // Si sigue preparada es que falló la sustitución, no la versión: otro arranque lo
        // intenta, pero no siempre (cada intento retrasa la ventana).
        if meta_path.exists() {
            meta.swap_failures = meta.swap_failures.saturating_add(1);
            if meta.swap_failures >= MAX_SWAP_FAILURES {
                log::warn!("[update] se descarta la {} tras {} intentos", meta.version, meta.swap_failures);
                discard_staged(target);
            } else {
                write_json(&meta_path, &meta);
            }
        }
        return Launch::Continue { notice: None };
    }
    match spawn_target(target, args, &["--updated-from".to_string(), from.clone()]) {
        Ok(()) => Launch::Exit,
        Err(e) => {
            // Pasó la autoprueba hace poco, pero ahora no arranca: se vuelve a esta, que sí
            // funciona (es la que corre), en vez de dejar un acceso directo que no abre nada.
            log::error!("[update] la {} no arranca ({e}); se vuelve a la {from}", meta.version);
            match read_json::<AppliedMeta>(&applied_path(target)) {
                Some(applied) => {
                    if let Err(e) = roll_back(target, &applied, &SWAP_GATE) {
                        log::error!("[update] no se pudo volver a la {from}: {e}");
                    }
                }
                None => log::error!("[update] sin {}: no se puede volver a la {from}", applied_path(target).display()),
            }
            Launch::Continue { notice: Some(format!("La versión {} no arrancaba; sigues con la {from}", meta.version)) }
        }
    }
}

/// La versión en uso funciona (la app lo dice a los `HEALTHY_AFTER` del primer fotograma): deja
/// de estar a prueba y se borra lo que ya no hace falta. En un hilo: borrar puede tardar si un
/// antivirus retiene el archivo.
pub fn mark_healthy() {
    let Some(target) = target_exe() else {
        return;
    };
    let probation = PROBATION.load(Ordering::Relaxed);
    let spawn = std::thread::Builder::new().name("nanofy-update-health".to_string()).spawn(move || {
        // Dentro de la puerta: una sustitución que empieza a la vez desde esta ventana no puede
        // colarse entre leer y reescribir nanofy.update.applied.json (la confirmación pisaría el
        // registro de la versión recién instalada) ni ver borrado el ejecutable recién apartado.
        let _gate = SWAP_GATE.lock().unwrap_or_else(|e| e.into_inner());
        mark_healthy_at(&target, probation, env!("CARGO_PKG_VERSION"));
    });
    if let Err(e) = spawn {
        log::warn!("[update] no se pudo lanzar la limpieza: {e}");
    }
}

/// Al cerrar con normalidad tras pintar la ventana, la versión a prueba también se da por buena:
/// cerrarla antes de los 20 s no es un fallo, y no debe acabar devolviéndola. Solo el json, que es
/// instantáneo; los borrados quedan para el siguiente arranque.
pub fn confirm_on_exit() {
    if PROBATION.load(Ordering::Relaxed) {
        if let Some(target) = target_exe() {
            confirm(&target, env!("CARGO_PKG_VERSION"));
        }
    }
}

fn confirm(target: &Path, this_version: &str) {
    let path = applied_path(target);
    if let Some(mut applied) = read_json::<AppliedMeta>(&path) {
        if !applied.healthy && applied.to == this_version {
            applied.healthy = true;
            if write_json(&path, &applied) {
                log::info!("[update] la {} funciona: deja de estar a prueba", applied.to);
            }
        }
    }
}

fn mark_healthy_at(target: &Path, probation: bool, this_version: &str) {
    if probation {
        confirm(target, this_version);
    }
    let applied_path = applied_path(target);
    let applied = read_json::<AppliedMeta>(&applied_path);
    // Si hay otra versión a prueba (esta ventana no es esa), su anterior es la vuelta atrás.
    let keep_old = applied.as_ref().is_some_and(|a| !a.healthy);
    for p in leftovers(target, !keep_old) {
        match std::fs::remove_file(&p) {
            Ok(()) => log::info!("[update] borrado {}", p.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // En uso (otra ventana con esa versión): en el siguiente arranque.
            Err(e) => log::debug!("[update] {} sigue en uso: {e}", p.display()),
        }
    }
    if let Some(a) = applied.filter(|a| a.healthy) {
        if !a.old.exists() {
            let _ = std::fs::remove_file(&applied_path);
        }
    }
}

/// Lo que se puede borrar junto a `target`: las versiones que no arrancaban, las anteriores
/// apartadas (`with_old`; también la que deja el instalador de 1.4–1.6, sin json) y los restos de
/// un intento fallido de 1.4–1.6 (un nanofy.new.exe vacío y nanofy-<v>.zip.part).
fn leftovers(target: &Path, with_old: bool) -> Vec<PathBuf> {
    let Some(dir) = target.parent() else {
        return Vec::new();
    };
    // Windows no distingue mayúsculas en los nombres; Linux sí.
    let norm = |s: &str| if cfg!(windows) { s.to_ascii_lowercase() } else { s.to_string() };
    let name_of = |p: &Path| p.file_name().map(|n| norm(&n.to_string_lossy())).unwrap_or_default();
    let mut aside = vec![name_of(&bad_exe_path(target))];
    if with_old {
        aside.push(name_of(&old_exe_path(target)));
    }
    let new_exe = name_of(&exe_sibling(target, "new"));
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = norm(&entry.file_name().to_string_lossy());
        let is_aside = aside.iter().any(|a| name == *a || is_numbered(&name, a));
        let legacy_part = name.starts_with("nanofy-") && name.ends_with(".zip.part");
        let legacy_new = name == new_exe && entry.metadata().is_ok_and(|m| m.is_file() && m.len() == 0);
        if is_aside || legacy_part || legacy_new {
            out.push(entry.path());
        }
    }
    out
}

/// `nanofy.old-1234.exe` es la variante con pid de `nanofy.old.exe` (ver `pick_aside`).
fn is_numbered(name: &str, base: &str) -> bool {
    let (stem, ext) = match base.strip_suffix(".exe") {
        Some(stem) => (stem, ".exe"),
        None => (base, ""),
    };
    name.strip_prefix(stem)
        .and_then(|r| r.strip_prefix('-'))
        .and_then(|r| r.strip_suffix(ext))
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

#[derive(Clone, Debug, PartialEq)]
pub enum UpdateResult {
    /// Hay una versión más reciente que la que se ejecuta.
    Available(UpdateInfo),
    /// Hay una versión más reciente, pero su release aún no trae zip para esta plataforma (CI
    /// subiendo los archivos o una compilación que falló). La app no avisa y vuelve a mirar en
    /// `PENDING_RECHECK`; tras `PENDING_MAX` seguidas avisa de que no hay descarga.
    Pending(UpdateInfo),
    /// La versión en ejecución es la última publicada (o más nueva, en desarrollo).
    UpToDate,
    /// No se pudo consultar (sin red, límite de GitHub, respuesta rara). Texto para Ajustes.
    Failed(String),
}

/// Espera hasta la siguiente consulta cuando la release aún no trae zip para esta plataforma.
pub const PENDING_RECHECK: Duration = Duration::from_secs(10 * 60);
/// Consultas seguidas sin zip a partir de las cuales se avisa de que esa versión no tiene
/// descarga para este sistema (en vez de seguir esperando sin decir nada).
pub const PENDING_MAX: u8 = 3;
/// Esperas antes de reintentar cuando la app prepara sola la versión nueva y se queda sin red;
/// agotadas, se avisa y vuelve la consulta normal de cada 6 h.
pub const AUTO_RETRY: [Duration; 2] = [Duration::from_secs(5 * 60), Duration::from_secs(30 * 60)];

/// Lanza la consulta en un hilo. `manual` = la pidió el usuario desde Ajustes (se le informa
/// también si está al día o si falló; la comprobación automática solo avisa de novedades).
pub fn check(ui: UiTx, manual: bool) {
    let current = current_version();
    let spawn = std::thread::Builder::new()
        .name("nanofy-update".to_string())
        .stack_size(256 * 1024)
        .spawn(move || {
            let result = fetch_latest(&current);
            // Sin las notas: pueden ser largas y no aportan nada al registro.
            match &result {
                UpdateResult::Available(i) => log::info!("[update] versión {current}: disponible {} · {:?}", i.version, i.asset),
                UpdateResult::Pending(i) => log::info!("[update] versión {current}: {} publicada sin zip para {}", i.version, platform_suffix()),
                other => log::info!("[update] versión {current}: {other:?}"),
            }
            // Si desde esta carpeta no puede actualizarse sola, el aviso lo dice de entrada en vez
            // de ofrecer «Instalar» (o prepararla sola) para fallar después. Aquí y no en la
            // interfaz: la prueba escribe un archivo junto al ejecutable.
            let (mut location, mut staged) = (None, None);
            if let (UpdateResult::Available(info), true, Some(exe)) = (&result, can_self_install(), target_exe()) {
                location = exe.parent().and_then(|dir| location_problem(&exe).map(|reason| (dir.to_path_buf(), reason)));
                if location.is_none() {
                    staged = already_staged(&exe, &info.version);
                }
            }
            ui.send(Msg::Update { result, manual, location, staged });
        });
    if let Err(e) = spawn {
        log::warn!("[update] no se pudo lanzar la comprobación: {e}");
    }
}

/// Dirección de la que se lee la última release. `NANOFY_UPDATE_URL` la sustituye por un
/// servidor de pruebas, pero solo si está en este equipo: una variable de entorno no debe
/// poder hacer que la app descargue e instale un ejecutable de un servidor ajeno.
fn release_url() -> String {
    let custom = std::env::var("NANOFY_UPDATE_URL").ok();
    if let Some(u) = custom.as_deref().filter(|u| !is_local_url(u)) {
        log::warn!("[update] NANOFY_UPDATE_URL ignorada ({u}): solo se admite http(s)://127.0.0.1 o localhost");
    }
    release_url_from(custom.as_deref())
}

fn release_url_from(custom: Option<&str>) -> String {
    match custom.map(str::trim).filter(|u| is_local_url(u)) {
        Some(u) => u.to_string(),
        None => format!("https://api.github.com/repos/{REPO}/releases/latest"),
    }
}

/// `http(s)://127.0.0.1…` o `http(s)://localhost…`, mirando el host entero: no basta con que
/// empiece igual (`localhost.example.com`) ni vale con usuario delante (`localhost@example.com`,
/// cuyo host real es el de después de la arroba).
fn is_local_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("http://").or_else(|| lower.strip_prefix("https://")) else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let host = match authority.split_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        Some(_) => return false,
        None => authority,
    };
    matches!(host, "127.0.0.1" | "localhost")
}

fn fetch_latest(current: &str) -> UpdateResult {
    fetch_latest_from(&release_url(), current)
}

/// La consulta en sí contra `url`: aparte para probarla con un servidor de este equipo, como el
/// de las pruebas de actualización de qa/.
fn fetch_latest_from(url: &str, current: &str) -> UpdateResult {
    let mut config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        );
    // Como la descarga (`download_agent`): con HTTP_PROXY en el entorno, el servidor de releases
    // de pruebas de este equipo se pediría al proxy, que no lo ve.
    if is_local_url(url) {
        config = config.proxy(None);
    }
    let agent = ureq::Agent::new_with_config(config.build());
    let resp = agent
        .get(url)
        .header("User-Agent", &format!("Nanofy/{current} (+https://github.com/{REPO})"))
        .header("Accept", "application/vnd.github+json")
        .call();
    let mut resp = match resp {
        Ok(r) if r.status().is_success() => r,
        // 404: todavía no hay ninguna release publicada.
        Ok(r) if r.status().as_u16() == 404 => return UpdateResult::UpToDate,
        Ok(r) => return UpdateResult::Failed(format!("GitHub respondió HTTP {}", r.status().as_u16())),
        Err(e) => return UpdateResult::Failed(format!("Sin conexión con GitHub ({e})")),
    };
    let json: serde_json::Value = match resp.body_mut().read_json() {
        Ok(v) => v,
        Err(e) => return UpdateResult::Failed(format!("Respuesta de GitHub no válida ({e})")),
    };
    interpret(&json, current)
}

/// Extrae la release del JSON de GitHub y decide si es más nueva que `current` (que debe ser
/// `current_version()`, para que `NANOFY_VERSION` sirva de verdad en las pruebas).
pub fn interpret(json: &serde_json::Value, current: &str) -> UpdateResult {
    let Some(tag) = json.get("tag_name").and_then(|v| v.as_str()) else {
        return UpdateResult::Failed("La release no tiene etiqueta".to_string());
    };
    let (Some(latest), Some(mine)) = (parse_version(tag), parse_version(current)) else {
        return UpdateResult::Failed(format!("Versión no reconocida: {tag}"));
    };
    if latest <= mine {
        return UpdateResult::UpToDate;
    }
    let version = format!("{}.{}.{}", latest.0, latest.1, latest.2);
    let page_url = json
        .get("html_url")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{RELEASES_URL}/tag/{tag}"));
    let assets: Vec<Asset> = json
        .get("assets")
        .and_then(|a| a.as_array())
        .map(|list| list.iter().filter_map(parse_asset).collect())
        .unwrap_or_default();
    let asset = pick_asset(&assets).cloned();
    let notes = clean_notes(json.get("body").and_then(|v| v.as_str()).unwrap_or(""));
    let info = UpdateInfo { version, page_url, notes, asset };
    if info.asset.is_some() {
        UpdateResult::Available(info)
    } else {
        UpdateResult::Pending(info)
    }
}

fn parse_asset(a: &serde_json::Value) -> Option<Asset> {
    Some(Asset {
        name: a.get("name")?.as_str()?.to_string(),
        url: a.get("browser_download_url")?.as_str()?.to_string(),
        size: a.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
        sha256: a.get("digest").and_then(|v| v.as_str()).and_then(parse_digest),
    })
}

/// El zip de esta plataforma: primero el nombre exacto que publica CI (`Nanofy-windows-x64.zip`)
/// y, si no está, cualquier .zip con el sufijo de la plataforma. Nunca el primero que contenga
/// el sufijo sin más, como hacen 1.1–1.6: un `Nanofy-windows-x64-setup.exe` se ordenaría antes
/// que el zip y se descargaría como si lo fuera.
fn pick_asset(assets: &[Asset]) -> Option<&Asset> {
    let suffix = platform_suffix();
    let exact = format!("Nanofy-{suffix}.zip");
    assets.iter().find(|a| a.name.eq_ignore_ascii_case(&exact)).or_else(|| {
        assets.iter().find(|a| {
            let name = a.name.to_ascii_lowercase();
            name.contains(suffix) && name.ends_with(".zip")
        })
    })
}

/// `sha256:` + 64 cifras hexadecimales (como publica GitHub el `digest`) → los 32 bytes. Otro
/// algoritmo, otra longitud o un carácter raro → `None`: se ignora y solo queda el tamaño.
fn parse_digest(s: &str) -> Option<[u8; 32]> {
    let (algo, hex) = s.trim().split_once(':')?;
    if !algo.eq_ignore_ascii_case("sha256") || hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (byte, pair) in out.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let hi = char::from(pair[0]).to_digit(16)?;
        let lo = char::from(pair[1]).to_digit(16)?;
        *byte = (hi * 16 + lo) as u8;
    }
    Some(out)
}

/// Marca de la línea que CI pone al principio de las notas para quien tiene 1.1–1.3 (cuyo aviso
/// solo sabe abrir el zip en el navegador, ver `release.yml`). A esta versión no le dice nada.
const LEGACY_NOTE_MARK: &str = "1.3 o anterior";

/// Notas completas sin la línea para 1.1–1.3 ni los blancos de alrededor.
fn clean_notes(body: &str) -> String {
    body.lines().filter(|l| !l.contains(LEGACY_NOTE_MARK)).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// Parte del nombre del zip que identifica esta plataforma (ver `.github/workflows/release.yml`).
pub fn platform_suffix() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows-x64"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "macos-arm64"
        } else {
            "macos-x64"
        }
    } else {
        "linux-x64"
    }
}

/// `a` es una versión posterior a `b` (dos que no se entienden no lo son).
pub fn is_newer(a: &str, b: &str) -> bool {
    matches!((parse_version(a), parse_version(b)), (Some(x), Some(y)) if x > y)
}

/// `v1.2.3`, `1.2.3` o `1.2.3-beta.1` → (1, 2, 3). Sin los tres números no es una versión.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versiones() {
        assert_eq!(parse_version("v1.0.0"), Some((1, 0, 0)));
        assert_eq!(parse_version("1.10.2"), Some((1, 10, 2)));
        assert_eq!(parse_version("2.0.0-beta.1"), Some((2, 0, 0)));
        assert_eq!(parse_version(" V0.9.12 "), Some((0, 9, 12)));
        assert_eq!(parse_version("1.0"), None);
        assert_eq!(parse_version("1.0.0.0"), None);
        assert_eq!(parse_version("latest"), None);
        assert!(parse_version("1.0.1") > parse_version("1.0.0"));
        assert!(parse_version("1.10.0") > parse_version("1.9.9"));
        assert!(parse_version("2.0.0") > parse_version("1.99.99"));
    }


    /// Digest real de Nanofy-windows-x64.zip de la v1.6.0, tal como lo da la API de GitHub.
    const DIGEST: &str = "sha256:99d1e8187c339653960b2136123cdc7140ee1f8c6049a1254562f1880d36c65f";

    fn release(tag: &str) -> serde_json::Value {
        serde_json::json!({
            "tag_name": tag,
            "html_url": format!("https://github.com/{REPO}/releases/tag/{tag}"),
            "body": "Arreglos varios",
            "assets": [
                {"name": "Nanofy-linux-x64.zip", "browser_download_url": "https://x/linux.zip", "size": 1001, "digest": DIGEST},
                {"name": "Nanofy-windows-x64.zip", "browser_download_url": "https://x/win.zip", "size": 1002, "digest": DIGEST},
                {"name": "Nanofy-macos-arm64.zip", "browser_download_url": "https://x/arm.zip", "size": 1003, "digest": DIGEST},
                {"name": "Nanofy-macos-x64.zip", "browser_download_url": "https://x/mac.zip", "size": 1004, "digest": DIGEST}
            ]
        })
    }

    /// Release con estos archivos (sin tamaño ni digest, como las anteriores a junio de 2025).
    fn release_with(tag: &str, names: &[&str]) -> serde_json::Value {
        let mut r = release(tag);
        r["assets"] = names
            .iter()
            .map(|n| serde_json::json!({"name": n, "browser_download_url": format!("https://x/{n}")}))
            .collect();
        r
    }

    fn available(r: &serde_json::Value) -> UpdateInfo {
        match interpret(r, "1.0.0") {
            UpdateResult::Available(info) => info,
            other => panic!("se esperaba una versión nueva, no {other:?}"),
        }
    }

    #[test]
    fn decide() {
        assert_eq!(interpret(&release("v1.0.0"), "1.0.0"), UpdateResult::UpToDate);
        // En desarrollo se puede ir por delante de la última release.
        assert_eq!(interpret(&release("v1.0.0"), "1.1.0"), UpdateResult::UpToDate);
        let info = available(&release("v1.0.1"));
        assert_eq!(info.version, "1.0.1");
        assert_eq!(info.page_url, format!("https://github.com/{REPO}/releases/tag/v1.0.1"));
        assert_eq!(info.notes, "Arreglos varios");
        let asset = info.asset.expect("zip de esta plataforma");
        let (url, size) = match platform_suffix() {
            "windows-x64" => ("win.zip", 1002),
            "linux-x64" => ("linux.zip", 1001),
            "macos-arm64" => ("arm.zip", 1003),
            _ => ("mac.zip", 1004),
        };
        assert!(asset.url.ends_with(url), "{asset:?}");
        assert_eq!(asset.name, format!("Nanofy-{}.zip", platform_suffix()));
        assert_eq!(asset.size, size);
        assert_eq!(asset.sha256, parse_digest(DIGEST));
        assert!(asset.sha256.is_some());
        assert!(matches!(interpret(&serde_json::json!({}), "1.0.0"), UpdateResult::Failed(_)));
        assert!(matches!(interpret(&release("nightly"), "1.0.0"), UpdateResult::Failed(_)));
    }

    /// Release sin zip para esta plataforma (CI aún subiendo, o esa compilación falló): no se
    /// avisa todavía, en vez de ofrecer el navegador.
    #[test]
    fn pendiente_sin_zip_de_esta_plataforma() {
        let others: Vec<&str> = ["Nanofy-linux-x64.zip", "Nanofy-windows-x64.zip", "Nanofy-macos-arm64.zip", "Nanofy-macos-x64.zip"]
            .into_iter()
            .filter(|n| !n.contains(platform_suffix()))
            .chain(["SHA256SUMS.txt"])
            .collect();
        assert_eq!(others.len(), 4);
        match interpret(&release_with("v1.0.1", &others), "1.0.0") {
            UpdateResult::Pending(info) => {
                assert_eq!(info.version, "1.0.1");
                assert_eq!(info.asset, None);
                assert_eq!(info.page_url, format!("https://github.com/{REPO}/releases/tag/v1.0.1"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(interpret(&release_with("v1.0.1", &[]), "1.0.0"), UpdateResult::Pending(_)));
        let mut r = release("v1.0.1");
        r.as_object_mut().unwrap().remove("assets");
        assert!(matches!(interpret(&r, "1.0.0"), UpdateResult::Pending(_)));
        // Un archivo sin URL de descarga no cuenta como zip.
        let mut r = release("v1.0.1");
        r["assets"] = serde_json::json!([{"name": format!("Nanofy-{}.zip", platform_suffix())}]);
        assert!(matches!(interpret(&r, "1.0.0"), UpdateResult::Pending(_)));
        // Sin zip, pero tampoco es más nueva: al día.
        assert_eq!(interpret(&release_with("v1.0.0", &others), "1.0.0"), UpdateResult::UpToDate);
    }

    /// El zip por su nombre exacto, aunque otro archivo con el sufijo de la plataforma vaya antes
    /// en la lista (la API ordena por nombre: «-» va antes que «.»).
    #[test]
    fn asset_exacto() {
        let s = platform_suffix();
        let names = ["SHA256SUMS.txt".to_string(), format!("Nanofy-{s}-setup.exe"), format!("Nanofy-{s}.zip")];
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let info = available(&release_with("v1.0.1", &names));
        assert_eq!(info.asset.as_ref().map(|a| a.name.clone()), Some(format!("Nanofy-{s}.zip")));
        // Sin tamaño ni digest en el JSON: desconocidos.
        assert_eq!(info.asset.as_ref().map(|a| (a.size, a.sha256)), Some((0, None)));

        // El exacto gana a otro .zip con el sufijo que vaya antes.
        let names = [format!("Nanofy-{s}-debug.zip"), format!("Nanofy-{s}.zip")];
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(available(&release_with("v1.0.1", &names)).asset.map(|a| a.name), Some(format!("Nanofy-{s}.zip")));

        // Sin el exacto vale otro .zip con el sufijo (como los de dist/), nunca el .exe.
        let names = [format!("Nanofy-{s}-setup.exe"), format!("Nanofy-1.0.1-{s}.ZIP")];
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(available(&release_with("v1.0.1", &names)).asset.map(|a| a.name), Some(format!("Nanofy-1.0.1-{s}.ZIP")));
        let names = [format!("Nanofy-{s}-setup.exe"), format!("Nanofy-{s}.sha256")];
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        assert!(matches!(interpret(&release_with("v1.0.1", &names), "1.0.0"), UpdateResult::Pending(_)));
    }

    #[test]
    fn digest() {
        let d = parse_digest(DIGEST).expect("digest válido");
        assert_eq!(d[0], 0x99);
        assert_eq!(d[1], 0xd1);
        assert_eq!(d[31], 0x5f);
        // En mayúsculas (cifras y algoritmo) es el mismo.
        assert_eq!(parse_digest(&DIGEST.to_ascii_uppercase()), Some(d));
        assert_eq!(parse_digest(&format!(" {DIGEST}\n")), Some(d));
        // Mal formados: se ignoran.
        assert_eq!(parse_digest(&DIGEST[..DIGEST.len() - 1]), None);
        assert_eq!(parse_digest(&format!("{DIGEST}0")), None);
        assert_eq!(parse_digest(&DIGEST.replace("99d1", "99g1")), None);
        assert_eq!(parse_digest(&DIGEST.replace("sha256", "sha512")), None);
        assert_eq!(parse_digest(DIGEST.trim_start_matches("sha256:")), None);
        assert_eq!(parse_digest(&DIGEST.replace("99d1", "9ñ1")), None);
        assert_eq!(parse_digest(""), None);
        // En la release: digest raro o nulo → None, sin perder el zip.
        let mut r = release("v1.0.1");
        for a in r["assets"].as_array_mut().unwrap() {
            a["digest"] = serde_json::json!("sha256:xyz");
        }
        assert_eq!(available(&r).asset.map(|a| a.sha256), Some(None));
        for a in r["assets"].as_array_mut().unwrap() {
            a["digest"] = serde_json::Value::Null;
        }
        assert_eq!(available(&r).asset.map(|a| a.sha256), Some(None));
    }

    /// Release tal como la publica `.github/workflows/release.yml`: la API de GitHub ordena los
    /// archivos por nombre y las versiones 1.1–1.6 se quedan con el PRIMERO cuyo nombre contiene
    /// su plataforma, así que ningún archivo añadido (SHA256SUMS.txt) puede llevar un sufijo de
    /// plataforma; si no, esas versiones lo descargarían como si fuera el zip.
    #[test]
    fn release_publicada_por_ci() {
        let names = [
            "Nanofy-linux-x64.zip",
            "Nanofy-macos-arm64.zip",
            "Nanofy-macos-x64.zip",
            "Nanofy-windows-x64.zip",
            "SHA256SUMS.txt",
        ];
        for suffix in ["windows-x64", "linux-x64", "macos-arm64", "macos-x64"] {
            assert_eq!(names.iter().filter(|n| n.contains(suffix)).count(), 1, "{suffix}");
        }
        let mut r = release_with("v1.0.1", &names);
        // Nota con la línea para 1.1–1.3 delante y el mensaje de la etiqueta detrás.
        r["body"] = serde_json::Value::String(
            "Si tu aviso solo muestra «Descargar» (Nanofy 1.3 o anterior), descomprime este zip y \
             sustituye tu nanofy.exe una sola vez; desde entonces se actualizará solo.\n\n\
             Nanofy 1.0.1: arreglos varios.\n\n- Uno\n- Dos"
                .to_string(),
        );
        let info = available(&r);
        assert_eq!(info.asset.map(|a| a.url), Some(format!("https://x/Nanofy-{}.zip", platform_suffix())));
        // Esta versión ya se actualiza sola: la línea para 1.1–1.3 no se le enseña.
        assert_eq!(info.notes, "Nanofy 1.0.1: arreglos varios.\n\n- Uno\n- Dos");
    }

    #[test]
    fn notas_completas_y_resumen() {
        let mut r = release("v9.9.9");
        r["body"] = serde_json::Value::String("x".repeat(1000));
        let info = available(&r);
        // Completas para el diálogo de novedades; el aviso flotante solo enseña el principio.
        assert_eq!(info.notes.chars().count(), 1000);
        assert_eq!(info.notes_preview().chars().count(), 401);
        assert!(info.notes_preview().ends_with('…'));
        // Cortas: el resumen es la nota entera, sin «…».
        assert_eq!(available(&release("v9.9.9")).notes_preview(), "Arreglos varios");
        // Saltos de línea de Windows y la línea antigua en medio.
        r["body"] = serde_json::Value::String("\r\nA\r\n(Nanofy 1.3 o anterior) x\r\nB\r\n".to_string());
        assert_eq!(available(&r).notes, "A\nB");
        r["body"] = serde_json::Value::Null;
        assert_eq!(available(&r).notes, "");
    }

    /// `NANOFY_UPDATE_URL` solo vale para un servidor de pruebas en este equipo.
    #[test]
    fn url_de_pruebas() {
        let github = format!("https://api.github.com/repos/{REPO}/releases/latest");
        assert_eq!(release_url_from(None), github);
        assert_eq!(release_url_from(Some("http://127.0.0.1:9")), "http://127.0.0.1:9");
        assert_eq!(release_url_from(Some(" http://127.0.0.1:9/latest ")), "http://127.0.0.1:9/latest");
        assert_eq!(release_url_from(Some("http://localhost:8790/latest?x=1")), "http://localhost:8790/latest?x=1");
        assert_eq!(release_url_from(Some("https://localhost/latest")), "https://localhost/latest");
        assert_eq!(release_url_from(Some("HTTP://LocalHost:9")), "HTTP://LocalHost:9");
        for bad in [
            "https://evil.example",
            "https://evil.example/?u=http://127.0.0.1",
            "http://127.0.0.1.evil.example/latest",
            "http://localhost.evil.example:9/",
            "http://localhost@evil.example/",
            "http://localhost:9@evil.example/",
            "http://127.0.0.1:/latest",
            "http://127.0.0.1:9x/latest",
            "ftp://127.0.0.1/latest",
            "127.0.0.1:9",
            "",
        ] {
            assert_eq!(release_url_from(Some(bad)), github, "{bad}");
        }
    }

    // ------------------------------------------------------------ preparación en segundo plano

    use std::io::{Read as _, Write as _};

    /// Carpeta temporal propia de cada prueba; se borra al terminar.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("nanofy-test-{}-{n}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Un «ejecutable» de esta plataforma: la cabecera buena y algo más de 1 MB de relleno.
    fn fake_exe() -> Vec<u8> {
        let mut v = exe_magic().to_vec();
        v.extend((0..1_200_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8));
        v
    }

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    /// Zip como el de la release: carpeta Nanofy/ con el ejecutable y el LEEME.
    fn release_zip(exe: &[u8]) -> Vec<u8> {
        let bin = format!("Nanofy/{}", binary_name_in_zip());
        zip_with(&[("Nanofy/LEEME.txt", b"hola".as_slice()), (bin.as_str(), exe)])
    }

    fn sha256(data: &[u8]) -> [u8; 32] {
        use sha2::Digest;
        let mut out = [0u8; 32];
        out.copy_from_slice(&sha2::Sha256::digest(data));
        out
    }

    /// Servidor HTTP mínimo en 127.0.0.1: a cada petición responde `body` anunciando `declared`
    /// bytes (si son más que `body`, la respuesta llega cortada).
    fn serve(body: Vec<u8>, declared: usize) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n");
                let _ = s.write_all(&body);
                let _ = s.flush();
                let _ = s.shutdown(std::net::Shutdown::Write);
            }
        });
        format!("http://{addr}/Nanofy-{}.zip", platform_suffix())
    }

    /// La consulta de punta a punta por HTTP contra un servidor de este equipo, con una release
    /// como la del servidor de las pruebas de qa/ (`fake_release.py`) y la de CI: el zip de esta
    /// plataforma con su tamaño y su digest entre los de los demás sistemas y SHA256SUMS.txt, y
    /// la línea para 1.1–1.3 delante de las notas.
    #[test]
    fn consulta_a_un_servidor_local() {
        let suffix = platform_suffix();
        let mut assets: Vec<serde_json::Value> = ["linux-x64", "macos-arm64", "macos-x64", "windows-x64"]
            .iter()
            .map(|p| {
                let mine = *p == suffix;
                serde_json::json!({
                    "name": format!("Nanofy-{p}.zip"),
                    "size": if mine { 7_784_787 } else { 22 },
                    "digest": if mine { DIGEST.to_string() } else { format!("sha256:{}", "0".repeat(64)) },
                    "browser_download_url": format!("http://127.0.0.1:9/download/v9.9.9/Nanofy-{p}.zip"),
                })
            })
            .collect();
        assets.push(serde_json::json!({"name": "SHA256SUMS.txt", "size": 300, "digest": null, "browser_download_url": "http://127.0.0.1:9/download/v9.9.9/SHA256SUMS.txt"}));
        let body = serde_json::json!({
            "tag_name": "v9.9.9",
            "html_url": "http://127.0.0.1:9/releases/tag/v9.9.9",
            "body": "Si tu aviso solo muestra «Descargar» (Nanofy 1.3 o anterior), descomprime este zip y sustituye tu nanofy.exe una sola vez; desde entonces se actualizará solo.\n\n## Versión de prueba",
            "assets": assets,
        })
        .to_string()
        .into_bytes();
        let len = body.len();
        let url = serve(body, len);
        let UpdateResult::Available(info) = fetch_latest_from(&url, "1.0.0") else {
            panic!("la release del servidor local no se vio como nueva");
        };
        assert_eq!(info.version, "9.9.9");
        assert_eq!(info.page_url, "http://127.0.0.1:9/releases/tag/v9.9.9");
        assert_eq!(info.notes, "## Versión de prueba");
        let asset = info.asset.expect("sin el zip de esta plataforma");
        assert_eq!(asset.name, format!("Nanofy-{suffix}.zip"));
        assert_eq!(asset.size, 7_784_787);
        assert_eq!(asset.sha256, parse_digest(DIGEST));
        // La misma versión que la publicada ya está al día.
        assert_eq!(fetch_latest_from(&url, "9.9.9"), UpdateResult::UpToDate);
    }

    /// Release 9.9.9 cuyo zip publicado (tamaño y SHA-256) es `zip`, servido en `url`.
    fn update_info(url: String, zip: &[u8]) -> UpdateInfo {
        UpdateInfo {
            version: "9.9.9".to_string(),
            page_url: format!("{RELEASES_URL}/tag/v9.9.9"),
            notes: String::new(),
            asset: Some(Asset { name: format!("Nanofy-{}.zip", platform_suffix()), url, size: zip.len() as u64, sha256: Some(sha256(zip)) }),
        }
    }

    /// Programa de prueba que hace de ejecutable nuevo: un .cmd en Windows (Rust lo lanza a
    /// través de cmd.exe) y un guion de sh en los demás.
    fn stub(dir: &Path, name: &str, windows: &str, unix: &str) -> PathBuf {
        if cfg!(target_os = "windows") {
            let p = dir.join(format!("{name}.cmd"));
            std::fs::write(&p, format!("@echo off\r\n{windows}\r\n")).unwrap();
            p
        } else {
            let p = dir.join(format!("{name}.sh"));
            std::fs::write(&p, format!("#!/bin/sh\n{unix}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            p
        }
    }

    const CURRENT_EXE: &[u8] = b"ejecutable en uso";

    /// Carpeta de la app con su ejecutable «en uso» y, aparte, la carpeta de estado.
    struct Setup {
        app: TempDir,
        state: TempDir,
        exe: PathBuf,
        /// Cerrojo propio: las pruebas corren a la vez y no deben esperarse entre sí.
        files: Mutex<()>,
    }

    /// Ningún intento más nuevo: el de la prueba es siempre el último.
    static ALWAYS_LATEST: fn() -> bool = || true;

    fn setup(name: &str) -> Setup {
        let app = TempDir::new(name);
        let state = TempDir::new(&format!("{name}-estado"));
        let exe = app.0.join(if cfg!(target_os = "windows") { "nanofy.exe" } else { "nanofy" });
        std::fs::write(&exe, CURRENT_EXE).unwrap();
        Setup { app, state, exe, files: Mutex::new(()) }
    }

    impl Setup {
        fn env<'a>(&'a self, test: &'a dyn Fn(&Path, &str) -> Result<(), String>) -> StageEnv<'a> {
            StageEnv { exe: self.exe.clone(), work_dir: self.state.0.join("update"), attempt: 1, files: &self.files, latest: &ALWAYS_LATEST, self_test: test }
        }

        fn run(&self, info: &UpdateInfo, test: &dyn Fn(&Path, &str) -> Result<(), String>) -> (Result<PathBuf, StageError>, Vec<Stage>) {
            let mut stages = Vec::new();
            let result = stage_blocking(&self.env(test), info, &AtomicBool::new(false), &mut |st| stages.push(st));
            (result, stages)
        }

        /// Lo que hay junto al ejecutable en uso (aparte de él) y en la carpeta de trabajo,
        /// ordenado. De paso comprueba que el ejecutable en uso sigue intacto.
        fn leftovers(&self) -> Vec<PathBuf> {
            let mut out: Vec<PathBuf> = [self.app.0.clone(), self.state.0.join("update")]
                .iter()
                .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()))
                .filter(|p| *p != self.exe)
                .collect();
            out.sort();
            assert_eq!(std::fs::read(&self.exe).unwrap(), CURRENT_EXE, "el ejecutable en uso no se toca");
            out
        }
    }

    fn ok_test(_: &Path, _: &str) -> Result<(), String> {
        Ok(())
    }

    fn no_test(_: &Path, _: &str) -> Result<(), String> {
        panic!("no debe llegar a la autoprueba")
    }

    /// Todo bien: el ejecutable nuevo queda junto al actual, probado y con su json, y no queda
    /// nada más (ni el zip ni la extracción).
    #[test]
    fn prepara_y_deja_lista() {
        let s = setup("lista");
        let exe = fake_exe();
        let zip = release_zip(&exe);
        let info = update_info(serve(zip.clone(), zip.len()), &zip);
        let staged = staged_exe_path(&s.exe);
        let meta_path = staged_meta_path(&s.exe);
        // Una descarga a medias abandonada (la app se cerró a mitad hace horas) no queda.
        let stale = s.state.0.join("update").join("Nanofy-9.9.8.1234-7.zip.part");
        std::fs::create_dir_all(s.state.0.join("update")).unwrap();
        std::fs::write(&stale, b"a medias").unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
        std::fs::OpenOptions::new().write(true).open(&stale).unwrap().set_modified(old).unwrap();
        let tested = std::cell::Cell::new(false);
        let test = |p: &Path, v: &str| -> Result<(), String> {
            // Se prueba el ejecutable ya en su sitio y entero, antes de darlo por listo.
            assert_eq!(p, staged);
            assert_eq!(v, "9.9.9");
            assert_eq!(std::fs::read(p).unwrap(), exe);
            assert!(!meta_path.exists());
            tested.set(true);
            Ok(())
        };
        let (result, stages) = s.run(&info, &test);
        assert_eq!(result.unwrap(), staged);
        assert!(tested.get());
        assert_eq!(std::fs::read(&staged).unwrap(), exe);
        let meta: StagedMeta = serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
        assert_eq!(
            meta,
            StagedMeta {
                version: "9.9.9".to_string(),
                target: s.exe.clone(),
                exe_size: exe.len() as u64,
                exe_sha256: hex(&sha256(&exe)),
                swap_failures: 0
            }
        );
        assert_eq!(stages.first(), Some(&Stage::Downloading { done: 0, total: Some(zip.len() as u64) }));
        assert!(stages.contains(&Stage::Preparing));
        assert!(stages.iter().all(|st| !matches!(st, Stage::Failed { .. } | Stage::Ready { .. } | Stage::NeedsMove { .. })));
        let mut want = vec![staged.clone(), meta_path.clone()];
        want.sort();
        assert_eq!(s.leftovers(), want);
        // Una consulta posterior la encuentra lista sin descargarla otra vez, y sin tocar nada.
        assert_eq!(already_staged(&s.exe, "9.9.9"), Some(staged.clone()));
        assert_eq!(already_staged(&s.exe, "9.9.10"), None);
        assert_eq!(s.leftovers(), want);
        // Cambiada después de probarla: no vale (se prepara de nuevo).
        std::fs::write(&staged, b"cambiada").unwrap();
        assert_eq!(already_staged(&s.exe, "9.9.9"), None);
    }

    /// SHA-256 distinto del publicado: «dañada» y no queda nada, tampoco lo que hubiera
    /// preparado un intento anterior.
    #[test]
    fn digest_distinto_descarta_todo() {
        let s = setup("digest");
        let zip = release_zip(&fake_exe());
        let mut info = update_info(serve(zip.clone(), zip.len()), &zip);
        info.asset.as_mut().unwrap().sha256 = Some([7; 32]);
        std::fs::write(staged_exe_path(&s.exe), b"version preparada antes").unwrap();
        std::fs::write(staged_meta_path(&s.exe), b"{}").unwrap();
        let (result, _) = s.run(&info, &no_test);
        assert!(matches!(result, Err(StageError::Fail(FailKind::Corrupt, _))), "{result:?}");
        assert_eq!(s.leftovers(), Vec::<PathBuf>::new());
    }

    /// Una descarga que se corta es cosa de la red; una más larga que la publicada, otro archivo.
    #[test]
    fn descarga_cortada_o_de_mas() {
        let s = setup("cortada");
        let zip = release_zip(&fake_exe());
        let half = zip[..zip.len() / 2].to_vec();
        // Content-Length completo y la mitad de los bytes.
        let (result, _) = s.run(&update_info(serve(half.clone(), zip.len()), &zip), &no_test);
        assert!(matches!(result, Err(StageError::Fail(FailKind::Network, _))), "{result:?}");
        assert_eq!(s.leftovers(), Vec::<PathBuf>::new());
        // El servidor da la respuesta por completa, pero faltan bytes según la release.
        let (result, _) = s.run(&update_info(serve(half.clone(), half.len()), &zip), &no_test);
        assert!(matches!(result, Err(StageError::Fail(FailKind::Network, _))), "{result:?}");
        assert_eq!(s.leftovers(), Vec::<PathBuf>::new());
        // Más bytes de los que publica la release.
        let mut longer = zip.clone();
        longer.extend_from_slice(&[0; 1000]);
        let (result, _) = s.run(&update_info(serve(longer.clone(), longer.len()), &zip), &no_test);
        assert!(matches!(result, Err(StageError::Fail(FailKind::Corrupt, _))), "{result:?}");
        assert_eq!(s.leftovers(), Vec::<PathBuf>::new());
    }

    /// El ejecutable se encuentra aunque el zip use «\» y mayúsculas, como hacen algunos
    /// compresores de Windows.
    #[test]
    fn zip_con_barras_de_windows() {
        assert!(is_binary_entry("Nanofy/nanofy.exe", "nanofy.exe"));
        assert!(is_binary_entry("Nanofy\\NANOFY.EXE", "nanofy.exe"));
        assert!(is_binary_entry("nanofy.exe", "nanofy.exe"));
        assert!(!is_binary_entry("Nanofy/nanofy.exe/", "nanofy.exe"));
        assert!(!is_binary_entry("Nanofy/nanofy.exe.bak", "nanofy.exe"));
        assert!(!is_binary_entry("Nanofy/viejo-nanofy.exe", "nanofy.exe"));
        let s = setup("barras");
        let exe = fake_exe();
        let name = format!("Nanofy\\{}", binary_name_in_zip().to_ascii_uppercase());
        let zip = zip_with(&[(name.as_str(), exe.as_slice())]);
        let (result, _) = s.run(&update_info(serve(zip.clone(), zip.len()), &zip), &ok_test);
        assert_eq!(std::fs::read(result.unwrap()).unwrap(), exe);
    }

    /// Zips sin un ejecutable que valga: sin él, demasiado pequeño, sin la cabecera de esta
    /// plataforma, con los datos dañados (no cuadra su CRC) o ni siquiera un zip.
    #[test]
    fn zip_sin_ejecutable_valido() {
        let s = setup("sin-exe");
        let bin = format!("Nanofy/{}", binary_name_in_zip());
        let small = fake_exe()[..500_000].to_vec();
        let mut no_magic = fake_exe();
        no_magic[0] = b'X';
        let mut damaged = release_zip(&fake_exe());
        let mid = damaged.len() / 2;
        damaged[mid] ^= 0xFF;
        let cases = [
            zip_with(&[("Nanofy/LEEME.txt", b"hola".as_slice())]),
            zip_with(&[(bin.as_str(), small.as_slice())]),
            zip_with(&[(bin.as_str(), no_magic.as_slice())]),
            damaged,
            b"esto no es un zip ".repeat(1000),
        ];
        for zip in cases {
            let (result, stages) = s.run(&update_info(serve(zip.clone(), zip.len()), &zip), &no_test);
            assert!(matches!(result, Err(StageError::Fail(FailKind::Corrupt, _))), "{result:?}");
            assert!(stages.contains(&Stage::Preparing));
            assert_eq!(s.leftovers(), Vec::<PathBuf>::new());
        }
    }

    /// La versión nueva responde otra versión en la autoprueba: «bloqueada», y el ejecutable
    /// preparado se borra.
    #[test]
    fn autoprueba_con_otra_version_bloquea() {
        let s = setup("otra-version");
        let wrong = stub(&s.state.0, "otra", "echo nanofy 0.0.1", "echo 'nanofy 0.0.1'");
        let zip = release_zip(&fake_exe());
        let test = |p: &Path, v: &str| -> Result<(), String> {
            assert!(p.exists());
            self_test(&wrong, v, Duration::from_secs(30))
        };
        let (result, _) = s.run(&update_info(serve(zip.clone(), zip.len()), &zip), &test);
        match result {
            Err(StageError::Fail(FailKind::Blocked, detail)) => assert!(detail.contains("nanofy 0.0.1"), "{detail}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(s.leftovers(), Vec::<PathBuf>::new());
    }

    /// Con la autoprueba de verdad, un archivo que solo tiene la cabecera de un ejecutable no
    /// arranca: «bloqueada» y sin restos.
    #[test]
    fn autoprueba_de_un_falso_ejecutable() {
        let s = setup("falso");
        let zip = release_zip(&fake_exe());
        let test = |p: &Path, v: &str| self_test(p, v, Duration::from_secs(30));
        let (result, _) = s.run(&update_info(serve(zip.clone(), zip.len()), &zip), &test);
        assert!(matches!(result, Err(StageError::Fail(FailKind::Blocked, _))), "{result:?}");
        assert_eq!(s.leftovers(), Vec::<PathBuf>::new());
    }

    /// La autoprueba en sí: vale la versión justa; otra versión, un error, un archivo que ya no
    /// está o uno que no responde, no.
    #[test]
    fn autoprueba() {
        let dir = TempDir::new("autoprueba");
        let limit = Duration::from_secs(30);
        let good = stub(&dir.0, "buena", "echo nanofy 9.9.9", "echo 'nanofy 9.9.9'");
        assert_eq!(self_test(&good, "9.9.9", limit), Ok(()));
        let other = stub(&dir.0, "otra", "echo nanofy 9.9.8", "echo 'nanofy 9.9.8'");
        assert!(self_test(&other, "9.9.9", limit).unwrap_err().contains("9.9.8"));
        let failing = stub(&dir.0, "falla", "echo nanofy 9.9.9\r\nexit 3", "echo 'nanofy 9.9.9'\nexit 3");
        assert!(self_test(&failing, "9.9.9", limit).unwrap_err().contains("error"));
        assert!(self_test(&dir.0.join("no-esta.exe"), "9.9.9", limit).unwrap_err().contains("desapareció"));
        let hung = stub(&dir.0, "colgada", ":otra\r\ngoto otra", "exec sleep 30");
        let t0 = Instant::now();
        let err = self_test(&hung, "9.9.9", Duration::from_secs(1)).unwrap_err();
        assert!(err.contains("no respondió"), "{err}");
        assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    }

    /// Cancelar a media descarga: para enseguida, no llega a prepararla y no deja nada.
    #[test]
    fn cancelar_a_media_descarga() {
        let s = setup("cancelar");
        let zip = release_zip(&fake_exe());
        let info = update_info(serve(zip.clone(), zip.len()), &zip);
        let cancel = AtomicBool::new(false);
        let mut stages = Vec::new();
        let result = stage_blocking(&s.env(&no_test), &info, &cancel, &mut |st| {
            if matches!(st, Stage::Downloading { done, .. } if done > 0) {
                cancel.store(true, Ordering::Relaxed);
            }
            stages.push(st);
        });
        assert!(matches!(result, Err(StageError::Cancelled)), "{result:?}");
        assert!(!stages.contains(&Stage::Preparing));
        assert_eq!(s.leftovers(), Vec::<PathBuf>::new());
    }

    /// Un intento cancelado que se atasca y llega tarde (ya se pidió otro) no toca lo que deja el
    /// nuevo, ni su ejecutable preparado ni su json; y uno que ya no es el último ni empieza.
    #[test]
    fn intento_sustituido_no_borra_lo_del_nuevo() {
        let s = setup("sustituido");
        let zip = release_zip(&fake_exe());
        let info = update_info(serve(zip.clone(), zip.len()), &zip);
        let staged = staged_exe_path(&s.exe);
        let meta = staged_meta_path(&s.exe);
        let latest = AtomicBool::new(true);
        let is_latest = || latest.load(Ordering::SeqCst);
        let env = StageEnv { latest: &is_latest, ..s.env(&no_test) };
        let result = stage_blocking(&env, &info, &AtomicBool::new(false), &mut |st| {
            // A media descarga se pide otra preparación, que deja su versión lista.
            if matches!(st, Stage::Downloading { done, .. } if done > 0) && latest.swap(false, Ordering::SeqCst) {
                std::fs::write(&staged, b"preparada por el intento nuevo").unwrap();
                std::fs::write(&meta, br#"{"nuevo":true}"#).unwrap();
            }
        });
        assert!(matches!(result, Err(StageError::Cancelled)), "{result:?}");
        assert_eq!(std::fs::read(&staged).unwrap(), b"preparada por el intento nuevo");
        assert_eq!(std::fs::read(&meta).unwrap(), br#"{"nuevo":true}"#);
        // Su descarga a medias no queda.
        let mut want = vec![staged.clone(), meta.clone()];
        want.sort();
        assert_eq!(s.leftovers(), want);
        // Pedido y ya sustituido antes de empezar: no quita nada.
        let never = || false;
        let env = StageEnv { latest: &never, ..s.env(&no_test) };
        let mut stages = Vec::new();
        let result = stage_blocking(&env, &info, &AtomicBool::new(false), &mut |st| stages.push(st));
        assert!(matches!(result, Err(StageError::Cancelled)), "{result:?}");
        assert!(stages.is_empty());
        assert_eq!(s.leftovers(), want);
    }

    /// «Cancelar» borra las descargas a medias de ese intento y nada más; al empezar otro se
    /// borran las abandonadas, pero no las recientes (pueden ser de otra ventana descargando).
    #[test]
    fn borrar_descargas_a_medias() {
        let dir = TempDir::new("partes");
        let names = |dir: &Path| -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        };
        let (zip3, exe3) = attempt_parts(&dir.0, "9.9.9", 3);
        let (zip4, _) = attempt_parts(&dir.0, "9.9.9", 4);
        for p in [&zip3, &exe3, &zip4, &dir.0.join("otra.txt")] {
            std::fs::write(p, b"x").unwrap();
        }
        remove_attempt_downloads(&dir.0, "9.9.9", 3);
        let zip4_name = zip4.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(names(&dir.0), [zip4_name.clone(), "otra.txt".to_string()]);
        // Reciente: puede ser de otra ventana que descarga ahora mismo.
        remove_stale_downloads(&dir.0, STALE_PART);
        assert_eq!(names(&dir.0), [zip4_name, "otra.txt".to_string()]);
        remove_stale_downloads(&dir.0, Duration::ZERO);
        assert_eq!(names(&dir.0), ["otra.txt"]);
        // Sin carpeta no hay nada que borrar.
        remove_stale_downloads(&dir.0.join("no-existe"), Duration::ZERO);
        remove_attempt_downloads(&dir.0.join("no-existe"), "9.9.9", 1);
    }

    /// «Actualizar automáticamente» solo en una copia instalada: nunca en una compilación de
    /// desarrollo (tampoco la de target\release), en una sesión de capturas o en el modo de
    /// control, salvo este contra un servidor de releases local.
    #[test]
    fn actualizar_solo_en_una_copia_instalada() {
        let installed = Path::new("C:/Users/ana/AppData/Local/Programs/Nanofy/nanofy.exe");
        let base = AutoGuard { debug: false, ephemeral: false, control: false, test_server: false, exe: Some(installed) };
        assert!(auto_update_allowed(&base));
        assert!(!auto_update_allowed(&AutoGuard { debug: true, ..base }));
        assert!(!auto_update_allowed(&AutoGuard { ephemeral: true, ..base }));
        assert!(!auto_update_allowed(&AutoGuard { control: true, ..base }));
        assert!(auto_update_allowed(&AutoGuard { control: true, test_server: true, ..base }));
        assert!(auto_update_allowed(&AutoGuard { test_server: true, ..base }));
        assert!(!auto_update_allowed(&AutoGuard { exe: None, ..base }));
        let mut dev = vec![
            "C:/src/spotify-app/target/release/nanofy.exe",
            "C:/src/spotify-app/target/debug/nanofy.exe",
            "C:/src/spotify-app/TARGET/Release/deps/nanofy-0123abcd.exe",
            "/home/ana/nanofy/target/x86_64-unknown-linux-gnu/release/nanofy",
        ];
        if cfg!(windows) {
            dev.push(r"C:\Users\Agustin\Documents\spotify-app\target\release\nanofy.exe");
        }
        for p in dev {
            assert!(is_dev_build_path(Path::new(p)), "{p}");
            // Ni con el servidor de pruebas: la compilación con la que se trabaja no se sustituye.
            assert!(!auto_update_allowed(&AutoGuard { control: true, test_server: true, exe: Some(Path::new(p)), ..base }), "{p}");
        }
        for p in [
            "C:/Users/ana/AppData/Local/Programs/Nanofy/nanofy.exe",
            "C:/Users/ana/target/nanofy.exe",
            "D:/Release/nanofy.exe",
            "C:/Users/ana/Documents/target-release/nanofy.exe",
            "C:/Users/ana/AppData/Local/Temp/nanofy-qa-8790/nanofy.exe",
        ] {
            assert!(!is_dev_build_path(Path::new(p)), "{p}");
        }
    }

    /// Al reiniciar se abre la nueva con los mismos argumentos (p. ej. --control), en la página
    /// actual en vez de la de arranque, y sin arrastrar los de una actualización anterior.
    #[test]
    fn argumentos_al_reiniciar() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        let before = args(&["--control", "8790", "--page", "loadctx:spotify:album:x", "--updated-from", "1.6.0", "--resume-playing", "--wait-pid", "12"]);
        let base = restart_args(&before, Some("settings"));
        // Lo que recibe de verdad la nueva: `spawn_target` quita los de paso y añade los de ahora.
        let sent: Vec<String> = strip_transient_args(&base).into_iter().chain(args(&["--updated-from", "1.7.0", "--resume-playing"])).collect();
        assert_eq!(sent, args(&["--control", "8790", "--page", "settings", "--updated-from", "1.7.0", "--resume-playing"]));
        // En el inicio no hace falta página; la de arranque (una prueba) no se repite.
        assert_eq!(restart_args(&args(&["--page", "stall", "--tab", "liked"]), None), args(&["--tab", "liked"]));
        // Sin valor no se come el argumento siguiente.
        assert_eq!(restart_args(&args(&["--page", "--control", "1"]), Some("library")), args(&["--control", "1", "--page", "library"]));
    }

    #[test]
    fn mas_nueva() {
        assert!(is_newer("1.7.0", "1.6.0"));
        assert!(is_newer("v1.10.0", "1.9.9"));
        assert!(!is_newer("1.7.0", "1.7.0"));
        assert!(!is_newer("1.6.0", "1.7.0"));
        assert!(!is_newer("nightly", "1.0.0"));
        assert!(!is_newer("1.0.0", "nightly"));
    }

    /// Desde dentro del zip (y de las carpetas de WinRAR o 7-Zip) no puede actualizarse; desde
    /// cualquier otra carpeta, también dentro de %TEMP% como las pruebas de QA, sí.
    #[test]
    fn ubicacion() {
        for p in [
            r"C:\Users\ana\AppData\Local\Temp\Temp1_Nanofy-windows-x64.zip\Nanofy\nanofy.exe",
            r"C:\Users\ana\AppData\Local\Temp\Temp1_Nanofy.zip\Nanofy\nanofy.exe",
            r"C:\Users\ana\AppData\Local\Temp\Temp2_Nanofy.zip\nanofy.exe",
            r"C:\Users\ana\AppData\Local\Temp\Rar$EXa12345.6789\Nanofy\nanofy.exe",
            r"C:\Users\ana\AppData\Local\Temp\7zO4A1B2C3D\nanofy.exe",
            "/tmp/Temp1_Nanofy-linux-x64.zip/Nanofy/nanofy",
        ] {
            assert_eq!(location_problem(Path::new(p)), Some(MoveReason::RunningFromZip), "{p}");
        }
        for p in [
            r"C:\Users\ana\AppData\Local\Temp\nanofy-upd\nanofy.exe",
            r"C:\Users\ana\AppData\Local\Temp\nanofy-qa-8790\nanofy.exe",
            r"D:\Programas\Nanofy\nanofy.exe",
            r"C:\Temp\Nanofy\nanofy.exe",
            r"C:\Users\ana\Temp1\nanofy.exe",
            r"C:\Users\ana\Temp_viejo\nanofy.exe",
            r"C:\Users\ana\7zOtros\nanofy.exe",
            "/home/ana/.local/bin/nanofy",
        ] {
            assert!(!running_from_zip(Path::new(p)), "{p}");
        }
        let dir = TempDir::new("ubicacion");
        assert_eq!(location_problem(&dir.0.join("nanofy.exe")), None);
        // La prueba de escritura no deja nada.
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
    }

    /// Una carpeta sin permiso de escritura: hay que mover la app. Como root se escribe igual y
    /// no hay nada que comprobar.
    #[cfg(unix)]
    #[test]
    fn carpeta_de_solo_lectura() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("solo-lectura");
        std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o555)).unwrap();
        let writable = std::fs::write(dir.0.join("x"), b"").is_ok();
        let found = location_problem(&dir.0.join("nanofy"));
        let _ = std::fs::remove_file(dir.0.join("x"));
        std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !writable {
            assert_eq!(found, Some(MoveReason::ReadOnly));
        }
    }

    /// Abierto desde dentro del zip ni se descarga nada: el aviso dice qué hacer.
    #[test]
    fn desde_el_zip_no_se_descarga() {
        let dir = TempDir::new("desde-zip");
        let app = dir.0.join("Temp1_Nanofy-windows-x64.zip").join("Nanofy");
        std::fs::create_dir_all(&app).unwrap();
        let files = Mutex::new(());
        let env = StageEnv { exe: app.join("nanofy.exe"), work_dir: dir.0.join("update"), attempt: 1, files: &files, latest: &ALWAYS_LATEST, self_test: &no_test };
        let info = update_info("http://127.0.0.1:9/no-se-pide".to_string(), b"zip");
        let mut stages = Vec::new();
        let result = stage_blocking(&env, &info, &AtomicBool::new(false), &mut |st| stages.push(st));
        match result {
            Err(StageError::Move(d, MoveReason::RunningFromZip)) => assert_eq!(d, app),
            other => panic!("{other:?}"),
        }
        assert!(stages.is_empty());
        assert!(!dir.0.join("update").exists());
        assert!(MoveReason::RunningFromZip.text(&app).contains("Descomprímelo"));
        assert!(MoveReason::ReadOnly.text(&app).contains(&app.display().to_string()));
    }

    /// Un archivo que otro proceso retiene un momento (como hace un antivirus) se renombra en
    /// cuanto lo suelta.
    #[cfg(windows)]
    #[test]
    fn renombrar_con_reintentos() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = TempDir::new("renombrar");
        let (from, to) = (dir.0.join("a.exe"), dir.0.join("b.exe"));
        std::fs::write(&from, b"x").unwrap();
        // Sin compartir nada: el renombrado da «en uso por otro proceso» mientras siga abierto.
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&from).unwrap();
        assert!(std::fs::rename(&from, &to).is_err());
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            drop(held);
        });
        rename_retrying(&from, &to).unwrap();
        release.join().unwrap();
        assert!(to.exists() && !from.exists());
    }

    #[test]
    fn estados() {
        assert!(Stage::Downloading { done: 0, total: None }.busy());
        assert!(Stage::Preparing.busy());
        assert!(Stage::Ready { version: "9.9.9".to_string(), staged: PathBuf::new() }.busy());
        assert!(!Stage::Idle.busy());
        assert!(!Stage::Available.busy());
        assert!(!Stage::Failed { kind: FailKind::Blocked, detail: String::new() }.busy());
        assert_eq!(Stage::Downloading { done: 50, total: Some(200) }.label(), "Descargando… 25 %");
        assert_eq!(Stage::Downloading { done: 2_500_000, total: None }.label(), "Descargando… 2.5 MB");
        // Lista no es instalada: espera a «Reiniciar» (o a abrir Nanofy otra vez).
        assert_eq!(Stage::Ready { version: "9.9.9".to_string(), staged: PathBuf::new() }.label(), "Nanofy 9.9.9 está lista");
    }

    // ------------------------------------------------------------ sustitución, arranque y salud

    const OLD_EXE: &[u8] = b"version anterior";
    const NEW_EXE: &[u8] = b"version nueva, ya probada";

    /// Carpeta con el ejecutable en uso (`OLD_EXE`) y la versión 9.9.9 preparada a su lado, como
    /// la deja `stage`.
    fn prepared(name: &str) -> (TempDir, PathBuf, StagedMeta) {
        let dir = TempDir::new(name);
        let target = dir.0.join(if cfg!(target_os = "windows") { "nanofy.exe" } else { "nanofy" });
        std::fs::write(&target, OLD_EXE).unwrap();
        std::fs::write(staged_exe_path(&target), NEW_EXE).unwrap();
        let meta = StagedMeta {
            version: "9.9.9".to_string(),
            target: target.clone(),
            exe_size: NEW_EXE.len() as u64,
            exe_sha256: hex(&sha256(NEW_EXE)),
            swap_failures: 0,
        };
        assert!(write_json(&staged_meta_path(&target), &meta));
        (dir, target, meta)
    }

    fn apply(target: &Path, meta: &StagedMeta) -> Result<(), String> {
        apply_staged_in(meta, target, "1.0.0", &Mutex::new(false))
    }

    fn applied_of(target: &Path) -> Option<AppliedMeta> {
        read_json(&applied_path(target))
    }

    /// Bien: la nueva en su sitio, la anterior guardada para poder volver y la instalada a prueba.
    #[test]
    fn instala_la_preparada() {
        let (dir, target, meta) = prepared("instala");
        apply(&target, &meta).unwrap();
        let old = old_exe_path(&target);
        assert_eq!(std::fs::read(&target).unwrap(), NEW_EXE);
        assert_eq!(std::fs::read(&old).unwrap(), OLD_EXE);
        assert_eq!(
            applied_of(&target),
            Some(AppliedMeta { from: "1.0.0".to_string(), to: "9.9.9".to_string(), old: old.clone(), boots: 0, healthy: false })
        );
        // Ni la preparada, ni su json, ni el cerrojo.
        let mut left: Vec<PathBuf> = std::fs::read_dir(&dir.0).unwrap().flatten().map(|e| e.path()).collect();
        left.sort();
        let mut want = vec![target.clone(), old, applied_path(&target)];
        want.sort();
        assert_eq!(left, want);
    }

    /// La ruta del json puede venir con otras mayúsculas (cada proceso la lee de cómo lo abrieron).
    #[cfg(windows)]
    #[test]
    fn misma_ruta_con_otras_mayusculas() {
        let (_dir, target, mut meta) = prepared("mayusculas");
        meta.target = PathBuf::from(target.to_string_lossy().to_uppercase());
        apply(&target, &meta).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), NEW_EXE);
    }

    /// Una preparada que no vale no toca el ejecutable en uso y se descarta.
    #[test]
    fn preparada_que_no_vale() {
        type Spoil = fn(&Path, &mut StagedMeta);
        let cases: [(&str, Spoil); 5] = [
            ("ya no está", |t, _| std::fs::remove_file(staged_exe_path(t)).unwrap()),
            ("incompleta", |t, _| std::fs::write(staged_exe_path(t), &NEW_EXE[..5]).unwrap()),
            ("cambió después de probarla", |t, _| std::fs::write(staged_exe_path(t), NEW_EXE.to_ascii_uppercase()).unwrap()),
            ("es para", |t, m| m.target = t.with_file_name("otro.exe")),
            ("no es más nueva", |_, m| m.version = "1.0.0".to_string()),
        ];
        for (why, spoil) in cases {
            let (_dir, target, mut meta) = prepared("no-vale");
            spoil(&target, &mut meta);
            let err = apply(&target, &meta).unwrap_err();
            assert!(err.contains(why), "{why}: {err}");
            assert_eq!(std::fs::read(&target).unwrap(), OLD_EXE, "{why}");
            assert!(!staged_exe_path(&target).exists() && !staged_meta_path(&target).exists(), "{why}");
            assert!(!old_exe_path(&target).exists() && applied_of(&target).is_none(), "{why}");
        }
    }

    /// La comparación es con la versión en ejecución (`current_version`): con `NANOFY_VERSION`
    /// fingida se instala la misma compilación, como hacen las pruebas de QA; sin fingir, no.
    #[test]
    fn compara_con_la_version_en_ejecucion() {
        let this = env!("CARGO_PKG_VERSION");
        let (_dir, target, mut meta) = prepared("misma-version");
        meta.version = this.to_string();
        assert!(apply_staged_in(&meta, &target, this, &Mutex::new(false)).unwrap_err().contains("no es más nueva"));
        assert_eq!(std::fs::read(&target).unwrap(), OLD_EXE);
        let (_dir, target, mut meta) = prepared("version-fingida");
        meta.version = this.to_string();
        apply_staged_in(&meta, &target, "1.0.0", &Mutex::new(false)).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), NEW_EXE);
    }

    /// Lo mismo de punta a punta con la variable de entorno. Solo en Windows: en Linux, cambiar el
    /// entorno mientras otras pruebas resuelven direcciones (getaddrinfo lo lee) no es seguro.
    #[cfg(windows)]
    #[test]
    fn nanofy_version_fingida() {
        let this = env!("CARGO_PKG_VERSION");
        let (_dir, target, mut meta) = prepared("env-fingida");
        meta.version = this.to_string();
        let before = std::env::var("NANOFY_VERSION").ok();
        std::env::set_var("NANOFY_VERSION", "1.0.0");
        let result = apply_staged(&meta, &target);
        match before {
            Some(v) => std::env::set_var("NANOFY_VERSION", v),
            None => std::env::remove_var("NANOFY_VERSION"),
        }
        result.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), NEW_EXE);
        assert_eq!(applied_of(&target).map(|a| (a.from, a.to)), Some(("1.0.0".to_string(), this.to_string())));
    }

    /// Si la nueva no se puede colocar (un antivirus la tiene abierta sin dejar renombrarla), el
    /// actual vuelve a su sitio con sus bytes y la preparada se conserva para el próximo intento.
    #[cfg(windows)]
    #[test]
    fn segundo_renombrado_falla_y_vuelve_el_actual() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_dir, target, meta) = prepared("segundo-falla");
        // FILE_SHARE_READ: se puede leer (la comprobación del SHA-256), no renombrar.
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(staged_exe_path(&target)).unwrap();
        let t0 = Instant::now();
        let err = apply(&target, &meta).unwrap_err();
        drop(held);
        assert!(err.contains("no se pudo colocar"), "{err}");
        assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
        assert_eq!(std::fs::read(&target).unwrap(), OLD_EXE);
        assert!(!old_exe_path(&target).exists());
        assert!(applied_of(&target).is_none());
        assert_eq!(std::fs::read(staged_exe_path(&target)).unwrap(), NEW_EXE);
        assert!(staged_meta_path(&target).exists());
        assert!(!lock_path(&target).exists());
    }

    /// Un nanofy.old.exe que no se puede borrar (abierto por otra ventana con la versión de antes)
    /// no impide instalar: el actual se aparta con el pid en el nombre.
    #[cfg(windows)]
    #[test]
    fn anterior_abierto_usa_otro_nombre() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_dir, target, meta) = prepared("anterior-abierto");
        let old = old_exe_path(&target);
        std::fs::write(&old, b"de una actualizacion anterior").unwrap();
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&old).unwrap();
        apply(&target, &meta).unwrap();
        drop(held);
        let aside = with_pid(&old);
        assert!(aside.to_string_lossy().ends_with(&format!("nanofy.old-{}.exe", std::process::id())), "{}", aside.display());
        assert_eq!(std::fs::read(&target).unwrap(), NEW_EXE);
        assert_eq!(std::fs::read(&aside).unwrap(), OLD_EXE);
        assert_eq!(std::fs::read(&old).unwrap(), b"de una actualizacion anterior");
        assert_eq!(applied_of(&target).map(|a| a.old), Some(aside));
    }

    /// Lo mismo en cualquier sistema: en el sitio de nanofy.old.exe hay algo que no se puede borrar.
    #[test]
    fn anterior_imborrable_usa_otro_nombre() {
        let (_dir, target, meta) = prepared("anterior-imborrable");
        let old = old_exe_path(&target);
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("x"), b"x").unwrap();
        apply(&target, &meta).unwrap();
        assert_eq!(std::fs::read(with_pid(&old)).unwrap(), OLD_EXE);
        assert_eq!(std::fs::read(&target).unwrap(), NEW_EXE);
    }

    /// Con el cerrojo de otra ventana no se toca nada; uno abandonado hace más de 2 min se quita.
    #[test]
    fn cerrojo() {
        let (_dir, target, meta) = prepared("cerrojo");
        let lock = lock_path(&target);
        std::fs::write(&lock, b"4321").unwrap();
        let err = apply(&target, &meta).unwrap_err();
        assert!(err.contains("otra ventana"), "{err}");
        assert_eq!(std::fs::read(&target).unwrap(), OLD_EXE);
        assert!(staged_meta_path(&target).exists() && staged_exe_path(&target).exists());
        // El cerrojo es de la otra ventana: sigue ahí.
        assert!(lock.exists());
        let f = std::fs::OpenOptions::new().write(true).open(&lock).unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(180)).unwrap();
        drop(f);
        apply(&target, &meta).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), NEW_EXE);
        assert!(!lock.exists());
    }

    /// Con la app cerrándose (puerta cerrada) no empieza ninguna sustitución.
    #[test]
    fn puerta_cerrada() {
        let (_dir, target, meta) = prepared("puerta");
        let err = apply_staged_in(&meta, &target, "1.0.0", &Mutex::new(true)).unwrap_err();
        assert!(err.contains("cerrando"), "{err}");
        assert_eq!(std::fs::read(&target).unwrap(), OLD_EXE);
        assert!(staged_meta_path(&target).exists() && staged_exe_path(&target).exists());
        assert!(!lock_path(&target).exists());
    }

    /// Tres arranques sin llegar a darse por buena: vuelve la anterior, la mala queda apartada y
    /// apuntada en nanofy.update.bad.json.
    #[test]
    fn tres_arranques_sin_confirmar_vuelve_la_anterior() {
        let (_dir, target, meta) = prepared("vuelta");
        apply(&target, &meta).unwrap();
        let gate = Mutex::new(false);
        let applied = applied_of(&target).unwrap();
        assert_eq!(check_health(&target, "9.9.9", &gate), Health::Probation);
        assert_eq!(applied_of(&target).map(|a| a.boots), Some(1));
        assert_eq!(check_health(&target, "9.9.9", &gate), Health::Probation);
        assert_eq!(check_health(&target, "9.9.9", &gate), Health::RolledBack { from: "1.0.0".to_string(), to: "9.9.9".to_string() });
        assert_eq!(std::fs::read(&target).unwrap(), OLD_EXE);
        assert_eq!(std::fs::read(bad_exe_path(&target)).unwrap(), NEW_EXE);
        assert!(!applied.old.exists());
        assert!(applied_of(&target).is_none());
        assert_eq!(read_json::<BadMeta>(&bad_meta_path(&target)).map(|b| b.version), Some("9.9.9".to_string()));
        // La de antes arranca sin nada a prueba.
        assert_eq!(check_health(&target, "1.0.0", &gate), Health::Normal);
        assert!(!lock_path(&target).exists());
    }

    /// Dada por buena (a los 20 s o al cerrar con normalidad), los arranques ya no cuentan; un
    /// registro de otra versión se descarta.
    #[test]
    fn salud_confirmada_o_de_otra_version() {
        let gate = Mutex::new(false);
        let (_dir, target, meta) = prepared("confirmada");
        apply(&target, &meta).unwrap();
        assert_eq!(check_health(&target, "9.9.9", &gate), Health::Probation);
        confirm(&target, "9.9.9");
        assert_eq!(applied_of(&target).map(|a| a.healthy), Some(true));
        for _ in 0..5 {
            assert_eq!(check_health(&target, "9.9.9", &gate), Health::Normal);
        }
        assert_eq!(std::fs::read(&target).unwrap(), NEW_EXE);

        let (_dir, target, meta) = prepared("otra-version");
        apply(&target, &meta).unwrap();
        assert_eq!(check_health(&target, "9.9.8", &gate), Health::Normal);
        assert!(applied_of(&target).is_none());
        assert_eq!(std::fs::read(&target).unwrap(), NEW_EXE);
    }

    /// Al darla por buena se borran el anterior (también con pid), el que no arrancaba y los
    /// restos de 1.4–1.6, pero nunca la versión preparada ni el anterior de otra versión que aún
    /// está a prueba.
    #[test]
    fn limpieza_al_dar_por_buena() {
        let (dir, target, meta) = prepared("limpieza");
        apply(&target, &meta).unwrap();
        // Una versión preparada después (la siguiente), que no debe tocarse.
        std::fs::write(staged_exe_path(&target), NEW_EXE).unwrap();
        let old = old_exe_path(&target);
        let old_pid = with_pid(&old);
        let bad = bad_exe_path(&target);
        let new_empty = exe_sibling(&target, "new");
        let part = dir.0.join("nanofy-1.6.0.zip.part");
        let readme = dir.0.join("LEEME.txt");
        for p in [&old_pid, &bad, &part, &readme] {
            std::fs::write(p, b"x").unwrap();
        }
        std::fs::write(&new_empty, b"").unwrap();
        // Esta ventana no es la versión a prueba: solo se va lo que no es una vuelta atrás.
        mark_healthy_at(&target, false, "9.9.9");
        assert!(old.exists() && old_pid.exists());
        assert!(!bad.exists() && !part.exists() && !new_empty.exists());
        assert_eq!(applied_of(&target).map(|a| a.healthy), Some(false));
        // Esta sí lo es.
        mark_healthy_at(&target, true, "9.9.9");
        assert!(!old.exists() && !old_pid.exists());
        assert!(applied_of(&target).is_none());
        assert!(readme.exists() && staged_exe_path(&target).exists());
        assert_eq!(std::fs::read(&target).unwrap(), NEW_EXE);
    }

    /// El salto 1.6 → 1.7 lo hace el instalador de 1.6: deja nanofy.old.exe sin json. Se borra al
    /// darla por buena; un nanofy.new.exe con contenido no es un resto vacío y se deja.
    #[test]
    fn limpieza_tras_el_instalador_de_16() {
        let dir = TempDir::new("desde-16");
        let target = dir.0.join(if cfg!(target_os = "windows") { "nanofy.exe" } else { "nanofy" });
        std::fs::write(&target, NEW_EXE).unwrap();
        let old = old_exe_path(&target);
        std::fs::write(&old, OLD_EXE).unwrap();
        let new_full = exe_sibling(&target, "new");
        std::fs::write(&new_full, NEW_EXE).unwrap();
        mark_healthy_at(&target, false, "9.9.9");
        assert!(!old.exists());
        assert!(new_full.exists() && target.exists());
    }

    /// Al arrancar, una sustitución que no sale se reintenta en los siguientes arranques, pero no
    /// siempre: al tercero se descarta.
    #[test]
    fn instalar_al_arrancar_se_rinde_al_tercer_intento() {
        let (_dir, target, _meta) = prepared("al-arrancar");
        std::fs::write(lock_path(&target), b"4321").unwrap();
        for n in 1..MAX_SWAP_FAILURES {
            assert!(matches!(apply_at_launch(&target, &[]), Launch::Continue { notice: None }));
            assert_eq!(read_json::<StagedMeta>(&staged_meta_path(&target)).map(|m| m.swap_failures), Some(n));
        }
        assert!(matches!(apply_at_launch(&target, &[]), Launch::Continue { notice: None }));
        assert!(!staged_meta_path(&target).exists() && !staged_exe_path(&target).exists());
        assert_eq!(std::fs::read(&target).unwrap(), OLD_EXE);
    }

    /// Al arrancar, si la nueva ya en su sitio no llega a abrirse (aquí no es un ejecutable), se
    /// vuelve a la que corre en vez de dejar un acceso directo que no abre nada.
    #[test]
    fn instalar_al_arrancar_y_la_nueva_no_abre() {
        let (_dir, target, _meta) = prepared("no-abre");
        match apply_at_launch(&target, &["--control".to_string(), "1".to_string()]) {
            Launch::Continue { notice: Some(n) } => assert!(n.contains("9.9.9"), "{n}"),
            Launch::Continue { notice: None } => panic!("sin aviso"),
            Launch::Exit => panic!("no debía abrirse nada"),
        }
        assert_eq!(std::fs::read(&target).unwrap(), OLD_EXE);
        assert!(applied_of(&target).is_none());
        assert_eq!(read_json::<BadMeta>(&bad_meta_path(&target)).map(|b| b.version), Some("9.9.9".to_string()));
        assert!(!staged_meta_path(&target).exists());
    }

    /// Al arrancar solo se instala la preparada desde una versión que ya se dio por buena y si no
    /// se acaba de llegar de una actualización o de una vuelta atrás.
    #[test]
    fn instalar_al_arrancar_solo_desde_una_version_buena() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(install_at_launch_allowed(&args(&["--control", "8790"]), &Health::Normal));
        assert!(!install_at_launch_allowed(&args(&[]), &Health::Probation));
        assert!(!install_at_launch_allowed(&args(&["--updated-from", "1.6.0"]), &Health::Normal));
        assert!(!install_at_launch_allowed(&args(&["--update-failed", "x"]), &Health::Normal));
    }

    /// Los argumentos de paso de una actualización no se arrastran al siguiente reinicio; los
    /// demás (el modo de control, la página…) sí.
    #[test]
    fn argumentos_de_paso() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        assert_eq!(strip_transient_args(&args(&["--control", "8790", "--wait-pid", "12", "--updated-from", "1.6.0"])), args(&["--control", "8790"]));
        assert_eq!(
            strip_transient_args(&args(&[
                "--page",
                "settings",
                "--update-failed",
                "La versión 9.9.9 no arrancaba; se volvió a la 1.7.0",
                "--resume-playing",
                "--tab",
                "liked"
            ])),
            args(&["--page", "settings", "--tab", "liked"])
        );
        // Sin valor no se come el argumento siguiente.
        assert_eq!(strip_transient_args(&args(&["--wait-pid", "--control", "8790"])), args(&["--control", "8790"]));
        assert_eq!(strip_transient_args(&args(&["--updated-from"])), args(&[]));
        assert_eq!(arg_value(&args(&["--control", "1", "--wait-pid", "77"]), "--wait-pid"), Some("77"));
    }

    #[test]
    fn nombres_con_pid() {
        let pid = std::process::id();
        assert_eq!(with_pid(Path::new("a/nanofy.old.exe")), Path::new(&format!("a/nanofy.old-{pid}.exe")));
        assert_eq!(with_pid(Path::new("a/nanofy.old")), Path::new(&format!("a/nanofy.old-{pid}")));
        assert!(is_numbered("nanofy.old-123.exe", "nanofy.old.exe"));
        assert!(is_numbered("nanofy.old-123", "nanofy.old"));
        assert!(!is_numbered("nanofy.old-.exe", "nanofy.old.exe"));
        assert!(!is_numbered("nanofy.old-12a.exe", "nanofy.old.exe"));
        assert!(!is_numbered("nanofy.old.exe", "nanofy.old.exe"));
        assert!(!is_numbered("nanofy.older-1.exe", "nanofy.old.exe"));
    }

    // ------------------------------------------------------------ textos del aviso

    /// Cada tipo de fallo tiene su frase; el motivo real va aparte solo si dice algo más, y si es
    /// la misma frase con el motivo entre paréntesis, solo el motivo.
    #[test]
    fn mensajes_de_fallo() {
        let (head, extra) = fail_message(FailKind::Network, "Sin conexión con GitHub (timeout)", "1.6.0");
        assert_eq!(head, "No se pudo descargar la actualización. Revisa tu conexión.");
        assert_eq!(extra.as_deref(), Some("Sin conexión con GitHub (timeout)"));
        // El texto del estado de prueba ya es la frase: nada que añadir.
        assert_eq!(fail_message(FailKind::Network, "No se pudo descargar la actualización. Revisa tu conexión.", "1.6.0").1, None);

        let (head, extra) = fail_message(FailKind::Corrupt, "La descarga llegó dañada (su SHA-256 no es el que publica la release) y se descartó.", "1.6.0");
        assert_eq!(head, "La descarga llegó dañada y se descartó.");
        assert_eq!(extra.as_deref(), Some("su SHA-256 no es el que publica la release"));
        assert_eq!(fail_message(FailKind::Corrupt, "Zip no válido: x", "1.6.0").1.as_deref(), Some("Zip no válido: x"));

        // Disco lleno: solo la frase. Otro error al escribir no dice que falte espacio.
        assert_eq!(fail_message(FailKind::Disk, DISK_FULL_TEXT, "1.6.0"), ("No hay espacio suficiente en el disco.".to_string(), None));
        let (head, extra) = fail_message(FailKind::Disk, "No se pudo guardar la descarga: acceso denegado", "1.6.0");
        assert!(!head.contains("espacio"), "{head}");
        assert_eq!(extra.as_deref(), Some("No se pudo guardar la descarga: acceso denegado"));

        // La autoprueba: la frase de siempre con la versión en uso, y el motivo entre paréntesis.
        let who = if cfg!(target_os = "windows") { "Windows o tu antivirus bloqueó" } else { "El sistema no dejó arrancar" };
        let detail = format!("{who} la versión nueva (respondió «nanofy 0.0.1» (código 1)). Sigues usando la 1.6.0.");
        let (head, extra) = fail_message(FailKind::Blocked, &detail, "1.6.0");
        assert_eq!(head, format!("{who} la versión nueva. Sigues usando la 1.6.0."));
        assert_eq!(extra.as_deref(), Some("respondió «nanofy 0.0.1» (código 1)"));

        // La sustitución ya trae la frase entera.
        let swap = "No se pudo sustituir nanofy.exe (en uso). Se intentará de nuevo al abrir Nanofy.";
        assert_eq!(fail_message(FailKind::Swap, swap, "1.6.0"), (swap.to_string(), None));
        assert_eq!(fail_message(FailKind::NoAsset, NO_ASSET_TEXT, "1.6.0"), (NO_ASSET_TEXT.to_string(), None));
    }

    #[test]
    fn progreso_de_la_descarga() {
        let (frac, text) = download_progress(3_800_000, Some(7_600_000));
        assert_eq!(frac, Some(0.5));
        assert_eq!(text, "50 % · 3.8 de 7.6 MB");
        // Nunca más del 100 % aunque llegue algo de más antes de cortarse.
        assert_eq!(download_progress(8_000_000, Some(7_600_000)), (Some(1.0), "100 % · 7.6 de 7.6 MB".to_string()));
        assert_eq!(download_progress(1_300_000, None), (None, "1.3 MB".to_string()));
        assert_eq!(download_progress(0, Some(0)), (None, "0.0 MB".to_string()));
    }

    #[test]
    fn notas_sin_markdown() {
        let notes = "## Novedades\n\n- **Reiniciar** retoma la música\n* Aviso nuevo\n\n\n---\nTexto con `código`.\n#\n- \n";
        assert_eq!(
            note_lines(notes),
            vec![
                NoteLine::Heading("Novedades".into()),
                NoteLine::Gap,
                NoteLine::Bullet("Reiniciar retoma la música".into()),
                NoteLine::Bullet("Aviso nuevo".into()),
                NoteLine::Gap,
                NoteLine::Text("Texto con código.".into()),
            ]
        );
        // Un «#» pegado al texto no es un título.
        assert_eq!(note_lines("#1 en listas"), vec![NoteLine::Text("#1 en listas".into())]);
        assert!(note_lines("\n \n").is_empty());
        let info = UpdateInfo { version: "1.7.0".into(), page_url: String::new(), notes: notes.into(), asset: None };
        assert_eq!(info.preview_lines(2), vec!["Novedades".to_string(), "• Reiniciar retoma la música".to_string()]);
    }

    /// Las notas guardadas al preparar una versión solo valen para esa versión.
    #[test]
    fn notas_guardadas() {
        let dir = TempDir::new("notas");
        let work = dir.0.join("update");
        let info = UpdateInfo { version: "1.7.0".into(), page_url: "https://x/v1.7.0".into(), notes: "- Arreglos".into(), asset: None };
        assert_eq!(load_notes(&work, "1.7.0"), None);
        save_notes(&work, &info);
        assert_eq!(load_notes(&work, "1.7.0"), Some(info));
        assert_eq!(load_notes(&work, "1.7.1"), None);
    }
}
