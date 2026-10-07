# Contribuir a Nanofy

Los fallos y las propuestas van en [Issues](https://github.com/ElRobaMichis/nanofy/issues). La
interfaz, los comentarios del código y los mensajes de commit están en español.

## Informar de un fallo

Indica la versión (Ajustes → Acerca de Nanofy), el sistema y los pasos para reproducirlo. Adjunta
`nanofy.log` (sesión actual) y `nanofy.log.1` (la anterior), que están en `%LOCALAPPDATA%\nanofy\data`
(Windows), `~/.local/state/nanofy` (Linux) o `~/Library/Application Support/nanofy` (macOS).
Los registros incluyen tu nombre de usuario de Spotify y pueden incluir títulos de canciones;
revísalos antes de publicarlos.

## Compilar y probar

Los requisitos están en el [README](README.md#compilar). Antes de abrir un pull request:

```sh
cargo build --release
cargo test --release --bin nanofy
```

La CI ([`ci.yml`](.github/workflows/ci.yml)) ejecuta lo mismo en Windows en cada push a `main` y
en cada pull request. `--bin nanofy` incluye las pruebas de los módulos parcheados de `vendor/`,
pero no las de sus crates (las de `librespot-core` se conectan a Spotify).

### Windows sin permisos de administrador

Usa la toolchain GNU con el gcc de WinLibs:

```powershell
Invoke-WebRequest https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe -OutFile rustup-init.exe
.\rustup-init.exe -y --default-host x86_64-pc-windows-gnu --profile minimal
winget install --id BrechtSanders.WinLibs.POSIX.UCRT -e
.\build.cmd
```

`build.cmd` añade al PATH la carpeta en la que winget instala WinLibs, compila y abre Nanofy.
`build.sh` compila en Linux, macOS o Git Bash, sin abrirlo.

## Pull requests

- La CI no ejecuta rustfmt ni clippy; sigue el estilo del archivo que modifiques.
- La CI solo compila y prueba en Windows. Si tu cambio afecta a Linux o macOS, indica cómo lo has probado.
- Si cambias algo en `vendor/`, explica qué corrige respecto a librespot.
- Al contribuir aceptas que tu código se publique con la [licencia MIT](LICENSE) del proyecto.

## Estructura

| Ruta | Contenido |
|---|---|
| `src/main.rs`, `src/bus.rs` | Arranque, registro (`nanofy.log`) y canal de mensajes hacia la interfaz |
| `src/app/` | Interfaz (egui): estado, páginas, paneles y barra del reproductor |
| `src/shell.rs`, `src/raster.rs` | Ventana (winit + softbuffer) y rasterizado por CPU |
| `src/backend.rs` | Sesión de librespot, dispositivo Spotify Connect y reproductor |
| `src/api.rs` | Web API, endpoints internos, LRCLIB y géneros |
| `src/model.rs` | Modelos de datos |
| `src/pathfinder.rs` | Búsqueda |
| `src/webauth.rs` | Autorización de la biblioteca (OAuth con PKCE) |
| `src/config.rs` | Ajustes y rutas |
| `src/cache.rs` | Copia local de la biblioteca |
| `src/images.rs` | Portadas |
| `src/media.rs` | Teclas multimedia |
| `src/taskbar.rs` | Botones de la miniatura de la barra de tareas (Windows) |
| `src/fonts.rs` | Fuentes |
| `src/update.rs` | Búsqueda, descarga, instalación y vuelta atrás de actualizaciones |
| `vendor/` | `librespot-core`, `librespot-connect` y `librespot-playback` 0.8 con parches propios (`[patch.crates-io]` en `Cargo.toml`) |

## Publicar una versión

Por cada corrección o función nueva se incrementa la versión de `Cargo.toml` según
[SemVer](https://semver.org/lang/es/) (`MAYOR.MENOR.PARCHE`).

1. Incrementa `version` en `Cargo.toml`, compila para que se actualice `Cargo.lock` y haz commit de los dos.
2. Crea una etiqueta anotada con esa versión (su mensaje será la nota de la release, tras una línea
   fija para quien tenga la 1.3 o anterior) y súbela con `main`:
   ```sh
   git tag -a vX.Y.Z -m "Qué cambia en esta versión"
   git push origin main vX.Y.Z
   ```
3. [`release.yml`](.github/workflows/release.yml) compila para Windows (MSVC), Linux y macOS (Intel
   y Apple Silicon), comprueba el tamaño y el SHA-256 de lo subido y publica los zips con
   `SHA256SUMS.txt`. También se puede lanzar a mano desde Actions, indicando la etiqueta.

- Si la etiqueta no coincide con la versión de `Cargo.toml`, no se publica nada.
- Los zips de Windows y Linux son obligatorios; si falla una de las dos compilaciones de macOS, la
  versión se publica sin ese zip.
- No cambies los nombres de los zips ni subas otros archivos a la release: el actualizador elige el
  zip por su nombre y el workflow falla si encuentra otros.
- Una etiqueta con sufijo (`vX.Y.Z-rc.N`) se publica como pre-release: no llega a las
  actualizaciones automáticas y sirve para probar antes.
- `NANOFY_CLIENT_ID` es un secreto opcional del repositorio: si existe, se incrusta al compilar como
  Client ID por defecto de la app de desarrollador personal, que es opcional (Ajustes → Biblioteca →
  Avanzado). No hace falta para iniciar sesión.

`dist.cmd` crea en local un zip solo para Windows (`dist\Nanofy-<versión>-windows-x64.zip`, con la
toolchain GNU); no sustituye a la release.
