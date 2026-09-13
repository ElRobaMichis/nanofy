# Nanofy

Cliente de Spotify nativo, en Rust, **sin navegador y sin GPU**. Reproduce música en tu equipo
(aparece como dispositivo Spotify Connect), controla tus otros dispositivos, muestra letras
sincronizadas, gestiona playlists y se une a Jams.

| Medida en este PC (Windows 11, i7 16 hilos, RTX 3060) | Nanofy | Fastpotify | Spotify oficial |
|---|---|---|---|
| RAM sin sesión (privada / en el Administrador de tareas) | **12 / 17 MB** | 100–250 MB (según su README) | 600 MB – 1 GB+ |
| RAM con sesión y biblioteca cargada, en reposo | **18 / 11 MB** | | |
| RAM reproduciendo | **29 / 21 MB** | | |
| CPU en reposo (30 s) | **0 %** | | |
| CPU reproduciendo (un núcleo) | **1–2 %** (decodificación + audio) | | |
| Proceso creado → primer fotograma pintado | **25 ms** sin sesión, **32 ms** con sesión | | |
| Cierre (clic en la X → proceso terminado) | **17 ms** sin sesión, **46 ms** reproduciendo | | |
| Coste de un fotograma (1920×1040, scroll) | **1,7 ms** en listas, **2,3 ms** en Inicio (0,5 ms es la copia a pantalla) | | |
| Peticiones de red al iniciar sesión | 6 (la biblioteca sale de una instantánea local) | | |
| Driver gráfico cargado | No | Sí (wgpu/OpenGL) | Sí |
| Binario | 14 MB, un solo ejecutable | un solo ejecutable | ~300 MB |

La diferencia clave con Fastpotify: **una ventana egui vacía con OpenGL ya ocupa ~125 MB** en un
PC con NVIDIA porque el driver reserva esa memoria. Nanofy dibuja la interfaz con un rasterizador
de triángulos por software (`src/raster.rs`) y la presenta con `softbuffer`, así que el proceso
nunca carga el driver. egui solo repinta cuando hay entrada, animación o un cambio de estado.

> Para reproducir hace falta **Spotify Premium** (limitación de librespot y de Spotify).

## Descargar

Los ejecutables listos para usar están en la pestaña **Releases** del repositorio:
<https://github.com/ElRobaMichis/nanofy/releases>. Descarga el zip de tu sistema
(`Nanofy-windows-x64.zip`, `Nanofy-linux-x64.zip`, `Nanofy-macos-arm64.zip` o
`Nanofy-macos-x64.zip`), descomprímelo y abre Nanofy. El `LEEME.txt` incluido explica el
primer arranque. Cada release la compila GitHub Actions a partir del código de este repositorio.

**Aviso de versiones nuevas.** Al arrancar (y cada seis horas) Nanofy consulta la última release
de este repositorio con una sola petición anónima a la API de GitHub. Si hay una versión más
reciente aparece un aviso arriba a la derecha con «Descargar» (el zip de tu sistema), «Ver
novedades» y «Omitir esta versión». En Ajustes → Acerca de Nanofy está el botón «Buscar
actualizaciones» y el interruptor para desactivar el aviso automático. No se descarga ni se
instala nada solo: sustituyes el ejecutable cuando quieras.

## Interfaz

Inspirada en el rediseño de Spotify de la comunidad: fondo casi negro, contenido en tarjetas
redondeadas, barra superior con Inicio · Buscar · Historial (la pestaña Buscar se convierte en el
campo de búsqueda), biblioteca a la izquierda con iconos de línea (Fijados, Playlists, Me gusta,
Álbumes, Artistas, Historial), reproductor flotante abajo y páginas a dos columnas con tabla de
canciones e información a la derecha. Las tarjetas distinguen el tipo por su forma: las
playlists llevan una "pila" encima, los álbumes son cuadrados, los artistas redondos y Me gusta
es verde. Iconos vectoriales propios (`src/app/icons.rs`), sin emoji del sistema. Tipografía
Segoe UI (Windows) con negrita real para los títulos.

Pestañas: Inicio y Buscar son fijas; cada playlist, álbum, artista o perfil se abre en su
propia pestaña con historial (atrás/adelante). Clic derecho en cualquier tarjeta, fila o
elemento de la biblioteca → «Abrir en una nueva pestaña». Se cierran con la ×, el botón central
del ratón o Ctrl+W. La pestaña Buscar se expande como campo de texto al hacer clic (no al
pasar el ratón).

Inicio: filtros Todo, Música, Podcasts y Audiolibros, y las mismas estanterías personalizadas
que el Spotify oficial («Hecho para ti» con los Daily Mix, Discover Weekly y Release Radar,
«Tus artistas favoritos», «Tus mezclas», «Álbumes para ti», emisoras, novedades, podcasts…),
obtenidas del endpoint interno de inicio a través de la sesión de librespot, sin gastar cuota
de la Web API, y cacheadas en `home.json` para que aparezcan al instante. Cada estantería
tiene flechas para desplazarse y un menú para fijarla arriba u ocultarla (se recuerda en
ajustes). El botón de personalizar, a la derecha de los filtros, abre un panel para añadir
playlists de tu biblioteca como secciones, fijar, reordenar arrastrando, mostrar u ocultar cada
sección y permitir o no las filas recomendadas.

Reproductor: el fondo toma el color dominante de la portada mientras suena y se vuelve gris en
pausa (con transición). De izquierda a derecha: reproducir/pausar, anterior, siguiente,
aleatorio, repetir, tiempo, línea de progreso (gris con lo reproducido en blanco; la bolita
aparece al acercar el ratón; clic o arrastre para saltar), duración, volumen (misma línea con
bolita), portada,
título / artista / álbum, y a la derecha corazón, playlist, letra, dispositivos y «más»
(descargar, temporizador de apagado, compartir, radio de la canción, ver álbum, ver artista,
miniplayer, pantalla completa, Jam, atajos), un separador y la cola.

Barra lateral: Artistas, Fijados, Playlists (con «+»), Canciones que te gustan, Guardados
(episodios de podcast guardados), Álbumes, Carpetas (las carpetas de playlists de Spotify, leídas
del rootlist interno porque la Web API no las expone), Podcasts que sigues, Audiolibros e
Historial.

«Añadir a playlist» (desde cualquier fila, el reproductor, la selección múltiple o un álbum
entero) abre una ventana con buscador, «+ Nueva playlist» (se escribe el nombre en la misma
fila, Intro la crea y queda marcada), la lista de playlists y carpetas con un círculo de
selección (paloma verde al marcar; se pueden marcar varias), y un pie fijo con Cancelar y Listo.

Cola: cada fila tiene una × y arriba hay «Vaciar cola». Spotify no ofrece a las apps
externas ninguna operación para quitar canciones de la cola, así que Nanofy la reconstruye:
recarga el contexto actual en la misma canción y posición (la cola queda vacía) y vuelve a
añadir las que se conserven. Por eso solo se pueden quitar canciones añadidas a la cola desde
Nanofy y solo cuando la música suena en Nanofy.

Menú de canción (el mismo al hacer clic derecho en una fila, en la portada del reproductor o en
los tres puntos del reproductor): descargar, temporizador de apagado, ocultar canción (se atenúa
en las listas y se salta al reproducir; es local a Nanofy, Spotify no lo ofrece a apps externas),
compartir, agregar a la biblioteca (Me gusta o playlist), agregar a la cola, radio de la canción,
ver álbum y ver artista (submenú si hay varios). En el reproductor se añaden miniplayer y
pantalla completa, y en los tres puntos también Jam y atajos. Estilo: fondo gris, icono y texto
en blanco, 250 px de ancho.

En Windows, la miniatura de la barra de tareas (al pasar el ratón por el icono) muestra los
botones anterior, reproducir/pausar y siguiente, como el Spotify oficial (`src/taskbar.rs`,
ITaskbarList3 con iconos dibujados a mano y un subclass del WndProc para recibir los clics).

«Ir a la radio de la canción» (en cualquier menú de canción) abre en una pestaña nueva la
playlist de radio que genera Spotify para esa canción (resuelta por el protocolo interno):
misma página que cualquier radio del inicio, con su portada generada, «De Spotify», la
canción semilla en primer lugar y sin reproducir nada hasta que pulses play.

Ajustes en tarjetas (Reproducción, Apariencia y rendimiento, Web API, Cuenta, Acerca de) con
interruptores, píldoras para calidad/tema/fps, cajas de texto y botones en píldora, todo con
el mismo estilo que los diálogos. Versión 1.0.0.

Detalles pedidos por la comunidad e incluidos: corazón y «añadir a playlist» como dos botones
distintos (en cada fila y en el reproductor), duración total de la cola, historial de escucha,
pestañas en artista con vista de lista o cuadrícula, playlists fijadas, biblioteca con filtro
y orden, y modo claro.

Página de artista: cabecera con la imagen del artista, nombre y seguidores; botones de
reproducir, seguir, añadir las populares a una playlist o a la cola; pestañas Inicio, Álbumes,
Sencillos y EPs, Recopilatorios, Aparece en y Acerca de; y una lupa que filtra dentro del
artista. Inicio muestra «Tus más escuchadas» (las 5 canciones del artista que más has
reproducido en Nanofy, con un registro local `plays.json` sembrado con tu historial),
«Populares», la discografía, «Los fans también escuchan» y, a la derecha, la «Selección del
artista» (último lanzamiento), retrato, biografía y géneros. Las pestañas de discografía van
ordenadas por fecha descendente, en cuadrícula o en lista; en la lista cada álbum tiene
portada, año, canciones y duración, botones de acción y una flecha que despliega sus canciones
(y las pliega al segundo clic).

Página de álbum: título, línea de artista • año • canciones • duración; fila de botones
(reproducir, aleatorio, guardar en la biblioteca, añadir a la cola, descargar, compartir, más)
y, a la derecha, una lupa que filtra las canciones del álbum; tabla con «#», «Título» y
«Duración», y a la derecha la portada grande, chips de género y los perfiles de los artistas.
Al pasar el ratón por una canción el número se convierte en un botón de reproducir verde y
aparecen añadir a playlist, descargar, más y un círculo de selección. Se pueden seleccionar
varias canciones: la fila de botones se vuelve verde y actúa sobre la selección (Me gusta,
playlist, cola, descargar, compartir, más) y la lupa se sustituye por un círculo verde con un
menos que deselecciona todo.

Playlists, Canciones que te gustan, podcasts e historial usan la misma barra (con sus botones
propios: editar, guardar, fijar…), la misma selección múltiple y la misma lupa, con las columnas
«#», «Título», «Álbum» y «Duración».

«Descargar» guarda el audio en la caché de librespot (Ajustes → Caché de audio), de modo que
esas canciones se reproducen sin volver a bajarlas; el registro está en `downloads.json`.

## Rendimiento (medido el 13 sep 2026, Windows 11, release, mediana de 3 ejecuciones)

| | 1.2.0 | 1.3.0 |
|---|---|---|
| Cierre con sesión (clic en × → proceso terminado) | 375 ms | 46 ms |
| Fotograma en Inicio, scroll a 1920×1040 (medio / máximo) | 6,6 / 10 ms | 2,3 / 5 ms |
| Fotograma en Canciones que te gustan (medio / máximo) | 2,5 / 5 ms | 1,7 / 4 ms |
| Panel de letras abierto mientras suena | 20 fps, 15 % CPU | 2 fps, 1,6 % CPU |
| Memoria en el Administrador de tareas reproduciendo | 48 MB | 21 MB |
| Proceso creado → primer fotograma | 35 ms | 32 ms |

Cómo (1.3.0): el rasterizador reparte el fotograma en bandas horizontales entre hasta 8 hilos
(bandas finas que cada hilo toma según termina, así las barras vacías no desequilibran) y las
portadas opacas a escala 1:1 se copian fila a fila sin mezclar; al cerrar, los ficheros se
escriben en el mismo hilo (son pequeños) y se espera al aviso de desconexión como mucho 30 ms;
el panel de letras solo pide desplazarse cuando cambia la línea (antes lo pedía en cada
fotograma y la animación de egui no paraba nunca); y a los 6 s de cada cambio de pista se
devuelven al sistema las páginas de fichero que dejó la descarga.

### Medidas anteriores (4 sep 2026)

| | Antes | Ahora |
|---|---|---|
| Primer fotograma pintado | 58 ms | 20 ms |
| Ventana visible con contenido | 58 ms | 36 ms (16 ms son de Windows/DWM al mostrarla) |
| Cierre (clic en ×) | 143 ms | 34 ms |
| CPU en reposo | 0,1 % | 0 % |
| Memoria privada (ajustes / inicio) | 23 MB | 18 / 23 MB |
| Memoria en el Administrador de tareas | 44–54 MB | 13–15 MB |
| Hilos | 34 | 33 |

Cómo: la ventana se crea oculta (crearla visible cuesta 18 ms más), se pinta el primer fotograma
y entonces se muestra ya con contenido; la instantánea y el registro se escriben en hilos aparte
al cerrar y la espera al aviso de desconexión bajó a 10 ms; las portadas tienen un presupuesto de
16 MB, se liberan tras unos segundos sin verse y las cabeceras grandes se limitan a 1280 px; las
listas no se copian por fotograma y las secciones del inicio se calculan una vez; repintado a
1 Hz al reproducir; memoria consultada con la API de Windows en vez de `sysinfo`; un solo hilo
de trabajo de tokio; y a los 4 s se devuelven al sistema las páginas usadas solo al arrancar.

Lo que queda de memoria privada (~18 MB en la página de ajustes) es estructural: el búfer de la
ventana (3–8 MB según tamaño), la sesión de Spotify (TLS, websocket, tokio), pilas de hilos, el
atlas de fuentes (0,5–1 MB) y los datos de la biblioteca. Durante la reproducción se suma el
archivo de audio de la canción en curso (10 MB a 320 kbps, hasta 40 MB sin pérdida).

## Repartirlo

- **Windows**: `dist.cmd` compila en release y deja `dist\Nanofy-<versión>-windows-x64.zip`
  con solo `nanofy.exe` y `LEEME.txt`. No necesita instalador ni runtimes: solo DLLs del
  propio Windows 10/11. Si defines `NANOFY_CLIENT_ID` antes de compilar, ese Client ID queda
  como valor por defecto en Ajustes → Web API. Añade el correo de Spotify de cada persona en
  la app de desarrollador (dashboard → User Management, hasta 25 usuarios).
- **Mac y Linux**: no se pueden compilar desde Windows. `.github/workflows/release.yml` los
  compila en GitHub Actions al subir una etiqueta `v*` (Windows, Linux, macOS Intel y Apple
  Silicon) y publica los zips en una release. Los binarios no van firmados: en Mac hay que
  abrir la app con clic derecho → Abrir la primera vez.
- Todo el mundo necesita Spotify Premium (requisito de Spotify para apps externas).

## Qué hace

- **Reproduce en este equipo** como dispositivo Spotify Connect (librespot): gapless, hasta
  320 kbps, salida en flotante de 32 bits sin remuestreo, normalización opcional, caché de audio.
- **Sin pérdida (experimental)**: la opción «Sin pérdida» pide FLAC a Spotify (parche propio de
  librespot en `vendor/`). Si tu cuenta o la canción no lo ofrecen, cae a 320 kbps.
- **Controla otros dispositivos** (móvil, altavoz, otro PC) desde el selector: play/pausa,
  siguiente, anterior, posición, volumen, aleatorio y repetir. También trae la música a este equipo.
- **Letras sincronizadas** con la línea actual resaltada y clic para saltar a ese punto.
  Fuente: endpoint interno de Spotify cuando está disponible y, si no, LRCLIB (abierto).
- **Cola** de reproducción y «Añadir a la cola» desde cualquier canción.
- **Jam** (escuchar en grupo): crear, copiar el enlace de invitación, unirse con enlace o
  código, ver participantes, salir o finalizar. Usa una API interna de Spotify vía librespot.
- **Playlists**: crear, renombrar, describir, pública/privada, colaborativa, cambiar la
  imagen (diálogo o arrastrar un JPG/PNG a la página), añadir y quitar canciones, guardar y
  quitar playlists de otros de tu biblioteca.
- **Perfiles**: página de cualquier usuario (desde el propietario de una playlist o pegando
  su enlace), con sus playlists públicas y botón Seguir. Perfil propio desde tu nombre.
- **Artistas**: página con populares y discografía, botón Seguir, página «Artistas que sigues».
- **Biblioteca**: playlists, Canciones que te gustan, álbumes guardados, artistas seguidos.
- **Búsqueda** con filtros Todo, Canciones, Artistas, Álbumes, Playlists, Podcasts, Episodios,
  Audiolibros y Perfiles (Spotify no busca perfiles por nombre: el filtro Perfiles busca el
  nombre de usuario exacto). Pegar un enlace de Spotify en la búsqueda abre directamente la
  canción, álbum, artista, playlist, podcast, perfil o Jam.
- **Podcasts**: título, autor, botón Seguir y menú; lista «Todos los episodios» con orden
  (nuevos/antiguos), filtro (todos, descargados, guardados) y búsqueda; cada episodio con
  miniatura, título, fecha • duración, descripción y botones reproducir, guardar en tus
  episodios, añadir a playlist, descargar, compartir y más. A la derecha, portada, «Acerca de»
  y temas del podcast. La valoración con estrellas de los podcasts no está en ninguna API
  pública, así que no se muestra.
- **Teclas multimedia** del sistema (SMTC en Windows, MPRIS en Linux, Now Playing en macOS) con
  metadatos y portada, mediante `souvlaki`.
- **Listas virtualizadas** (una playlist de 10 000 canciones solo construye las filas
  visibles), portadas al tamaño exacto en que se muestran con caché en disco y desalojo LRU.
- **Volumen en decibelios**: el slider es lineal en dB (20·log10 de la amplitud, rango de 40 dB), así
  que cada tramo del slider cambia el volumen percibido por igual. El volumen se aplica en la
  salida de audio, no al decodificar, y el cambio se oye al instante. Rueda del ratón sobre el slider
  para ajustar en pasos de 2 %; la etiqueta muestra el valor en dB.
- **Fuentes del sistema** para chino, japonés, coreano, árabe y otros alfabetos, mapeadas en
  memoria (no se copian los archivos de fuente a la RAM).
- **Tema** claro/oscuro/sistema, escala, límite de fps y atajos de teclado para todo.

### Cómo se mantiene ligero

- Sin GPU: rasterizador por CPU con regla top-left, atlas de fuentes en un byte por texel y
  muestreo 1:1 (sin bilineal) para las portadas, que se piden ya al tamaño en que se dibujan.
- Repintado solo bajo demanda: 0 % de CPU en reposo. Durante la reproducción, 2 fotogramas por
  segundo para la barra de progreso; las letras se repintan solo cuando cambia la línea. Las
  animaciones sin interacción del usuario van a 24 fps como máximo; el límite alto (144 fps por
  defecto) solo se usa mientras mueves el ratón o el teclado. Con la ventana minimizada no se
  dibuja nada.
- El dispositivo de audio se abre en la primera reproducción, no al arrancar; las teclas
  multimedia del sistema se registran entonces. El volumen se aplica en la salida.
- Instantánea de la biblioteca en disco (`library.json`): la interfaz se llena al instante y la
  red solo refresca. Al iniciar sesión se hacen 6 peticiones en vez de más de 20.
- Cachés acotadas: 100 texturas en memoria con desalojo LRU, 100 MB de portadas en disco,
  256 MB de audio por defecto. Hilos con pilas de 512 KB y pocos hilos de bloqueo en tokio.
- Cierre en ~150 ms: se guarda el estado, Spirc avisa a Spotify y el proceso termina sin
  esperar a los destructores.
- Si Spotify agota la cuota de tu app (429 con espera larga), Nanofy deja de llamar a la Web API
  hasta que pase, lo recuerda entre reinicios y lo avisa una sola vez.

## Instalar y compilar

Necesitas Rust estable (1.85+) y un compilador de C (dependencias de TLS de librespot).

### Windows (sin permisos de administrador)

```powershell
Invoke-WebRequest https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe -OutFile rustup-init.exe
.\rustup-init.exe -y --default-host x86_64-pc-windows-gnu --profile minimal
winget install --id BrechtSanders.WinLibs.POSIX.UCRT -e
```

Después, desde la carpeta del proyecto:

```powershell
.\build.cmd
.\target\release\nanofy.exe
```

### Linux

```bash
# Debian/Ubuntu
sudo apt install build-essential pkg-config libasound2-dev libssl-dev libxkbcommon-dev libwayland-dev libdbus-1-dev
# Arch
sudo pacman -S --needed base-devel alsa-lib openssl libxkbcommon wayland dbus
cargo build --release
```

### macOS

```bash
xcode-select --install
cargo build --release
```

## Primer arranque: dos pasos

**1. Iniciar sesión con Spotify.** Se abre el navegador con la página de consentimiento de
Spotify (OAuth 2.0 con PKCE); Nanofy nunca ve tu contraseña. librespot guarda una credencial
reutilizable y no vuelves a ver el navegador. Con esto ya puedes reproducir, ver la cola, las
letras y usar Jam.

**2. Configurar tu Client ID (Ajustes → Web API).** Spotify limita globalmente la Web API
del client id compartido de librespot: responde `429 API rate limit exceeded` a todo. Por eso
la biblioteca, la búsqueda, las playlists y los perfiles necesitan una app propia, gratuita:

1. Entra en https://developer.spotify.com/dashboard y pulsa **Create app**.
2. Nombre y descripción libres. En **Redirect URIs** añade exactamente
   `http://127.0.0.1:8899/callback`. Marca **Web API** y guarda.
3. Copia el **Client ID**, pégalo en Ajustes → Web API y pulsa **Conectar**. Se abre el
   navegador una vez para autorizar; el token se guarda y se renueva solo.

Fastpotify resuelve lo mismo con una app compartida registrada por su autor. Nanofy no puede
distribuir una porque una app en modo desarrollo admite como máximo 25 usuarios.

Además, Spotify prohíbe a las apps en modo desarrollo varios endpoints (responden 403):
canciones de una playlist, perfiles de usuario, top tracks y discografía de un artista, y las
comprobaciones de «guardado» y «seguido». Nanofy obtiene esos datos por el protocolo interno
de librespot, así que funcionan igual; el estado de «Me gusta» y «Siguiendo» se deduce de tus
listas de Me gusta y de artistas seguidos, que se cargan al iniciar sesión.

Rutas:

- Ajustes: `%APPDATA%\nanofy\config\settings.json` / `~/.config/nanofy/settings.json`
- Credenciales, volumen y token de la Web API: `%LOCALAPPDATA%\nanofy\data` / `~/.local/state/nanofy`
- Caché de audio y portadas: `%LOCALAPPDATA%\nanofy\cache` / `~/.cache/nanofy`

## Atajos de teclado

| Atajo | Acción |
|---|---|
| Espacio | Reproducir / pausar |
| Ctrl+← / Ctrl+→ | Anterior / siguiente |
| Shift+← / Shift+→ | Retroceder / avanzar 10 s |
| Ctrl+↑ / Ctrl+↓ | Volumen |
| M | Silenciar |
| S / R | Aleatorio / repetir |
| Q / L | Cola / letra |
| Ctrl+J | Jam |
| Ctrl+N | Nueva playlist |
| Ctrl+F o / | Buscar (o pegar un enlace) |
| Ctrl+B | Mostrar u ocultar la barra lateral |
| Alt+← / Alt+→ | Atrás / adelante |
| Ctrl+H / Ctrl+L | Inicio / Canciones que te gustan |
| Ctrl+, | Ajustes |
| Ctrl+/ o ? | Lista de atajos |
| Ctrl+W | Cerrar la pestaña actual |
| Ctrl+Q | Salir |

En macOS, Cmd sustituye a Ctrl. Las teclas multimedia del teclado también funcionan.

## Opciones de línea de comandos

```
nanofy --page settings|liked|search|albums|artists   página inicial
nanofy --side queue|lyrics                           panel lateral abierto
nanofy --tab playlist:<id>                          abre pestañas adicionales (repetible)
nanofy --jam                                         abre la ventana de Jam
nanofy --diag                                        prueba automática (dispositivos, cola, letras,
                                                     10 s de reproducción a volumen bajo) y sale.
                                                     Gasta cuota de la Web API: no lo repitas muchas
                                                     veces seguidas (el modo desarrollo tiene una cuota
                                                     diaria pequeña).
```

Con `RUST_LOG=info,nanofy=debug` la versión de depuración (`cargo build`) escribe trazas en la
consola.

## Cómo está hecho

```
src/main.rs         arranque, icono, flags
src/shell.rs        ventana winit + egui-winit + softbuffer, bucle de eventos, límite de fps
src/raster.rs       rasterizador de mallas de egui por CPU (alfa premultiplicada, tramos, regla top-left)
src/fonts.rs        fuentes del sistema (CJK, árabe…) mapeadas en memoria
src/app/mod.rs      estado, mensajes, acciones, atajos, composición de la ventana
src/app/pages.rs    inicio, búsqueda, biblioteca, playlist, álbum, podcast, perfil, ajustes
src/app/artist.rs   página de artista (pestañas, búsqueda interna, lista desplegable)
src/app/panels.rs   cola, letras, Jam, editor de playlist, atajos
src/app/player_bar.rs  barra lateral, barra superior, reproductor, dispositivos
src/app/widgets.rs  portadas, tarjetas, chips, tabla de pistas, menús
src/app/icons.rs    iconos vectoriales de línea
src/app/theme.rs    paleta, tipografía y estilo de egui
src/backend.rs      librespot: sesión, Spotify Connect (Spirc), reproductor, OAuth de sesión
src/api.rs          Web API (ureq), endpoints internos vía spclient (Jam, letras), LRCLIB
src/webauth.rs      OAuth PKCE con la app del usuario, token guardado y renovado
src/media.rs        teclas multimedia y "Reproduciendo ahora" (souvlaki)
src/images.rs       descarga y caché de portadas, texturas con desalojo LRU
src/model.rs        modelos de la Web API, letras, Jam, enlaces
src/config.rs       rutas y settings.json
vendor/librespot-playback  librespot-playback 0.8 con la opción de pedir FLAC
```

Hilos: 1 de interfaz, 2 de tokio (librespot), 2 de Web API, 2 de imágenes, más los del audio.
Perfil de release con LTO, `codegen-units = 1`, `panic = "abort"` y símbolos eliminados.

### Decisiones

- **Rust** es la única opción realista: librespot es la implementación abierta del protocolo de
  Spotify y reescribirla en otro lenguaje llevaría años. La ganancia frente a Fastpotify viene de
  no cargar un driver gráfico, repintar solo bajo demanda y acotar cachés, no del lenguaje.
- **egui** por su tamaño y porque produce mallas simples que un rasterizador de 300 líneas
  puede dibujar. Cada textura vive una sola vez en RAM.
- **ureq** con el almacén de certificados del sistema, en hilos aparte; sin reqwest ni tokio para
  la Web API.

## Limitaciones conocidas

- Sin Client ID propio, la Web API no funciona (429 de Spotify). Reproducción, cola remota,
  letras y Jam sí.
- Letras: Spotify no sirve su endpoint de letras a este tipo de cliente; se usa LRCLIB, que no
  cubre todas las canciones.
- Jam y «Sin pérdida» dependen de APIs internas de Spotify no documentadas; pueden dejar de
  funcionar si Spotify las cambia. No se ha podido verificar que Spotify entregue FLAC a esta
  cuenta.
- Oyentes mensuales, ciudades con más oyentes, reproducciones por canción, «Merch» y «Features
  & more» solo existen en la API privada (GraphQL) del reproductor web de Spotify; ningún
  cliente externo puede obtenerlos, así que la página de artista no los muestra.
- Sin reordenar canciones arrastrando, sin icono de bandeja, sin Winamp/MilkDrop.
- Probado solo en Windows en esta versión.

Nanofy es un proyecto independiente y no está afiliado a Spotify. Spotify es una marca de
Spotify AB. Licencia MIT.
