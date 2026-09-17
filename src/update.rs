//! Aviso de versiones nuevas: consulta la última release publicada en GitHub y la compara con
//! la versión compilada. Una sola petición sin autenticación (límite de GitHub: 60 por hora e IP),
//! en un hilo aparte; el resultado llega a la interfaz por el bus.

use std::time::Duration;

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
    /// Zip de esta plataforma, si la release lo incluye.
    pub asset_url: Option<String>,
    /// Notas de la release (cuerpo en Markdown, recortado).
    pub notes: String,
}

/// Progreso de la instalación automática (se muestra en el aviso).
#[derive(Clone, Debug, PartialEq)]
pub enum InstallProgress {
    Downloading { done: u64, total: Option<u64> },
    Extracting,
    /// El ejecutable nuevo está listo en esta ruta (junto al actual).
    Ready(std::path::PathBuf),
    Failed(String),
}

impl InstallProgress {
    pub fn label(&self) -> String {
        match self {
            InstallProgress::Downloading { done, total: Some(t) } if *t > 0 => format!("Descargando… {} %", (done * 100 / t).min(100)),
            InstallProgress::Downloading { done, .. } => format!("Descargando… {:.1} MB", *done as f64 / 1e6),
            InstallProgress::Extracting => "Instalando…".to_string(),
            InstallProgress::Ready(_) => "Reiniciando…".to_string(),
            InstallProgress::Failed(e) => e.clone(),
        }
    }
}

/// `true` si en esta plataforma la app puede sustituirse a sí misma (ejecutable suelto).
pub fn can_self_install() -> bool {
    cfg!(any(target_os = "windows", target_os = "linux"))
}

/// Nombre del ejecutable dentro del zip de la release para esta plataforma.
fn binary_name_in_zip() -> &'static str {
    if cfg!(target_os = "windows") {
        "nanofy.exe"
    } else {
        "nanofy"
    }
}

/// Ruta del ejecutable viejo que queda tras una actualización (se borra al arrancar).
pub fn old_exe_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.with_extension(if cfg!(target_os = "windows") { "old.exe" } else { "old" }))
}

/// Borra el ejecutable anterior si quedó de una actualización (puede fallar si el proceso
/// viejo aún no ha terminado; se reintenta en el siguiente arranque).
pub fn cleanup_old_exe() {
    if let Some(old) = old_exe_path() {
        if old.exists() {
            match std::fs::remove_file(&old) {
                Ok(()) => log::info!("[update] ejecutable anterior borrado"),
                Err(e) => log::debug!("[update] el ejecutable anterior sigue en uso: {e}"),
            }
        }
    }
}

/// Descarga el zip de la release, extrae el ejecutable junto al actual y avisa cuando está listo.
pub fn install(ui: UiTx, info: UpdateInfo) {
    let spawn = std::thread::Builder::new()
        .name("nanofy-update-install".to_string())
        .spawn(move || {
            let result = install_blocking(&ui, &info);
            let progress = match result {
                Ok(path) => InstallProgress::Ready(path),
                Err(e) => {
                    log::warn!("[update] instalación fallida: {e}");
                    InstallProgress::Failed(e)
                }
            };
            ui.send(Msg::UpdateProgress(progress));
        });
    if let Err(e) = spawn {
        log::warn!("[update] no se pudo lanzar la instalación: {e}");
    }
}

fn install_blocking(ui: &UiTx, info: &UpdateInfo) -> Result<std::path::PathBuf, String> {
    use std::io::{Read, Write};
    let Some(url) = info.asset_url.clone() else {
        return Err("Esta release no trae un zip para tu sistema; descárgala desde GitHub".to_string());
    };
    let exe = std::env::current_exe().map_err(|e| format!("No se encuentra el ejecutable actual: {e}"))?;
    let dir = exe.parent().ok_or("Ruta del ejecutable sin carpeta")?.to_path_buf();
    let new_path = exe.with_extension(if cfg!(target_os = "windows") { "new.exe" } else { "new" });
    let zip_path = dir.join(format!("nanofy-{}.zip.part", info.version));
    // Comprobación de permisos antes de descargar nada.
    std::fs::File::create(&new_path).map_err(|e| format!("No se puede escribir en {}: {e}. Descarga el zip y sustituye el ejecutable a mano.", dir.display()))?;

    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .http_status_as_error(false)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut resp = agent
        .get(&url)
        .header("User-Agent", &format!("Nanofy/{} (+https://github.com/{REPO})", current_version()))
        .call()
        .map_err(|e| format!("Sin conexión con GitHub ({e})"))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub respondió HTTP {}", resp.status().as_u16()));
    }
    let total = resp
        .headers()
        .get("Content-Length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let mut reader = resp.body_mut().as_reader();
    let mut file = std::fs::File::create(&zip_path).map_err(|e| format!("No se pudo crear la descarga: {e}"))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    let mut last_report = 0u64;
    ui.send(Msg::UpdateProgress(InstallProgress::Downloading { done: 0, total }));
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("Descarga interrumpida: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("No se pudo guardar la descarga: {e}"))?;
        done += n as u64;
        if done - last_report >= 256 * 1024 {
            last_report = done;
            ui.send(Msg::UpdateProgress(InstallProgress::Downloading { done, total }));
        }
    }
    drop(file);
    if let Some(t) = total {
        if done != t {
            let _ = std::fs::remove_file(&zip_path);
            return Err(format!("Descarga incompleta ({done} de {t} bytes)"));
        }
    }
    ui.send(Msg::UpdateProgress(InstallProgress::Extracting));

    let extract = (|| -> Result<(), String> {
        let f = std::fs::File::open(&zip_path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(f).map_err(|e| format!("Zip no válido: {e}"))?;
        let want = binary_name_in_zip();
        let idx = (0..archive.len())
            .find(|&i| archive.by_index(i).map(|e| e.name().rsplit('/').next() == Some(want)).unwrap_or(false))
            .ok_or_else(|| format!("El zip no contiene {want}"))?;
        let mut entry = archive.by_index(idx).map_err(|e| e.to_string())?;
        let mut out = std::fs::File::create(&new_path).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("No se pudo extraer el ejecutable: {e}"))?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&zip_path);
    extract?;
    let size = std::fs::metadata(&new_path).map(|m| m.len()).unwrap_or(0);
    if size < 1_000_000 {
        let _ = std::fs::remove_file(&new_path);
        return Err("El ejecutable descargado no parece válido".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&new_path, std::fs::Permissions::from_mode(0o755));
    }
    Ok(new_path)
}

#[derive(Clone, Debug, PartialEq)]
pub enum UpdateResult {
    /// Hay una versión más reciente que la que se ejecuta.
    Available(UpdateInfo),
    /// La versión en ejecución es la última publicada (o más nueva, en desarrollo).
    UpToDate,
    /// No se pudo consultar (sin red, límite de GitHub, respuesta rara). Texto para Ajustes.
    Failed(String),
}

/// Lanza la consulta en un hilo. `manual` = la pidió el usuario desde Ajustes (se le informa
/// también si está al día o si falló; la comprobación automática solo avisa de novedades).
pub fn check(ui: UiTx, manual: bool) {
    let current = current_version();
    let spawn = std::thread::Builder::new()
        .name("nanofy-update".to_string())
        .stack_size(256 * 1024)
        .spawn(move || {
            let result = fetch_latest(&current);
            log::info!("[update] versión {current}: {result:?}");
            ui.send(Msg::Update { result, manual });
        });
    if let Err(e) = spawn {
        log::warn!("[update] no se pudo lanzar la comprobación: {e}");
    }
}

fn fetch_latest(current: &str) -> UpdateResult {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = agent
        .get(&url)
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

/// Extrae la release del JSON de GitHub y decide si es más nueva que `current`.
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
    let asset_url = json
        .get("assets")
        .and_then(|a| a.as_array())
        .and_then(|assets| {
            assets.iter().find_map(|a| {
                let name = a.get("name")?.as_str()?;
                if name.contains(platform_suffix()) {
                    a.get("browser_download_url")?.as_str().map(str::to_string)
                } else {
                    None
                }
            })
        });
    let mut notes = json
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if notes.chars().count() > 400 {
        notes = notes.chars().take(400).collect::<String>() + "…";
    }
    UpdateResult::Available(UpdateInfo { version, page_url, asset_url, notes })
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

    fn release(tag: &str) -> serde_json::Value {
        serde_json::json!({
            "tag_name": tag,
            "html_url": format!("https://github.com/{REPO}/releases/tag/{tag}"),
            "body": "Arreglos varios",
            "assets": [
                {"name": "Nanofy-linux-x64.zip", "browser_download_url": "https://x/linux.zip"},
                {"name": "Nanofy-windows-x64.zip", "browser_download_url": "https://x/win.zip"},
                {"name": "Nanofy-macos-arm64.zip", "browser_download_url": "https://x/arm.zip"},
                {"name": "Nanofy-macos-x64.zip", "browser_download_url": "https://x/mac.zip"}
            ]
        })
    }

    #[test]
    fn decide() {
        assert_eq!(interpret(&release("v1.0.0"), "1.0.0"), UpdateResult::UpToDate);
        // En desarrollo se puede ir por delante de la última release.
        assert_eq!(interpret(&release("v1.0.0"), "1.1.0"), UpdateResult::UpToDate);
        match interpret(&release("v1.0.1"), "1.0.0") {
            UpdateResult::Available(info) => {
                assert_eq!(info.version, "1.0.1");
                assert_eq!(info.page_url, format!("https://github.com/{REPO}/releases/tag/v1.0.1"));
                assert_eq!(info.notes, "Arreglos varios");
                let asset = info.asset_url.expect("zip de esta plataforma");
                assert!(asset.ends_with(match platform_suffix() {
                    "windows-x64" => "win.zip",
                    "linux-x64" => "linux.zip",
                    "macos-arm64" => "arm.zip",
                    _ => "mac.zip",
                }));
            }
            other => panic!("se esperaba una versión nueva, no {other:?}"),
        }
        // Sin zip para esta plataforma: se ofrece la página de la release.
        let mut r = release("v1.0.1");
        r["assets"] = serde_json::json!([]);
        match interpret(&r, "1.0.0") {
            UpdateResult::Available(info) => assert_eq!(info.asset_url, None),
            other => panic!("{other:?}"),
        }
        assert!(matches!(interpret(&serde_json::json!({}), "1.0.0"), UpdateResult::Failed(_)));
        assert!(matches!(interpret(&release("nightly"), "1.0.0"), UpdateResult::Failed(_)));
    }

    #[test]
    fn notas_recortadas() {
        let mut r = release("v9.9.9");
        r["body"] = serde_json::Value::String("x".repeat(1000));
        match interpret(&r, "1.0.0") {
            UpdateResult::Available(info) => assert_eq!(info.notes.chars().count(), 401),
            other => panic!("{other:?}"),
        }
    }
}
