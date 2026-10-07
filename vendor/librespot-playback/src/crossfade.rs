//! Primitivas del fundido entre canciones («crossfade»): la rampa de igual potencia, la canción
//! saliente que se mezcla bajo la entrante, cuándo y cuánto fundir, la regla de los álbumes de
//! Spotify y la protección contra el recorte mientras suenan dos canciones a la vez.
//!
//! Aquí no hay estado del reproductor: `player.rs` decide cuándo empieza y cuándo se corta cada
//! fundido y usa estas piezas. Todo trabaja sobre lo que dan los decodificadores (44,1 kHz
//! estéreo, f64 intercalado; `lib.rs` comprueba al compilar que las constantes coinciden), antes
//! del volumen y de la salida.
//!
//! Sin rutas `crate::`: el binario incluye este fichero con `#[path]` para que `cargo test` corra
//! sus pruebas (las de las dependencias no se ejecutan desde el binario). La única dependencia,
//! `librespot_metadata`, también la tiene el binario.

use std::collections::VecDeque;
use std::f64::consts::FRAC_PI_2;

use librespot_metadata::audio::UniqueFields;

/// Frecuencia del mezclador: la única que aceptan los decodificadores.
pub const SAMPLE_RATE: u32 = 44_100;
/// Canales del mezclador (estéreo intercalado).
pub const CHANNELS: usize = 2;

/// Tope del fundido, el mismo que el control de Spotify (0–12 s).
pub const CROSSFADE_MAX_MS: u32 = 12_000;
/// Un fundido de menos de un segundo no se oye como fundido sino como un corte raro: por debajo
/// la transición queda sin hueco (gapless), como hasta ahora.
pub const CROSSFADE_MIN_MS: u32 = 1_000;
/// Antelación con la que el reproductor pide el fundido. La carga de la siguiente canción vuelve
/// pasando por Spirc, que a veces está esperando una petición HTTP (notify, send_state): sin este
/// margen cada milisegundo de ese viaje acortaría el fundido, y uno lento lo anularía del todo.
pub const CROSSFADE_LEAD_MS: u32 = 750;
/// Suelta de la saliente cuando el fundido se corta (salto, otra canción, búsqueda): 40 ms no se
/// oyen como fundido y bastan para que no chasquee.
pub const RELEASE_FRAMES: u64 = 1_764;
/// Bloque que se escribe cuando solo suena la saliente (la entrante aún carga, o la suelta).
/// Pequeño para que la entrante pueda unirse enseguida; la cola de rodio ya da el colchón.
pub const TAIL_FRAMES: usize = 1_024;
/// Entrada mínima de la entrante (50 ms). Si llega tarde (estaba cargando) y a la saliente casi
/// no le queda rampa, empezar a ganancia plena sería un escalón audible.
pub const FADE_IN_MIN_FRAMES: u64 = 2_205;
/// Umbral del limitador de la mezcla. Más alto que el del nivel «Alto» (−1 dBFS): solo tiene que
/// atrapar lo que la suma de dos canciones pase de la escala, no comprimir cada una.
pub const MIX_LIMIT_THRESHOLD_DBFS: f64 = -0.5;
/// Rodilla del limitador de la mezcla: estrecha, para que empiece a actuar en −1 dBFS y no antes.
/// Con la de la normalización (5 dB) comprimiría desde −3 dBFS casi todo un máster moderno
/// mientras dura el fundido, también cuando una de las dos canciones ya casi no se oye.
pub const MIX_LIMIT_KNEE_DB: f64 = 1.0;
/// Hasta aquí `safety_clamp` no toca nada; por encima satura suave hacia 1,0.
pub const SAFETY_CLAMP_KNEE: f64 = 0.98;
/// Paquetes vacíos seguidos que se toleran antes de dar la saliente por acabada: un decodificador
/// que nunca da muestras no puede colgar el hilo del reproductor.
const MAX_EMPTY_PACKETS: u32 = 64;
/// Bloques que puede ocupar la suelta al cortar un fundido. Son dos (1764 marcos en bloques de
/// 1024); el tope solo garantiza que cortar nunca se quede escribiendo.
const MAX_RELEASE_BLOCKS: usize = 4;
/// Reducción del limitador de la mezcla por debajo de la cual se da por vuelto a la unidad. Quitar
/// 0,001 dB de golpe es un salto de una diezmilésima de la señal (−79 dB): inaudible, y a partir
/// de ahí lo que suena vuelve a ser bit a bit lo de sin fundido.
const LIMITER_SETTLED_DB: f64 = 1e-3;

/// Milisegundos a marcos del mezclador (redondeo hacia abajo).
pub fn ms_to_frames(ms: u32) -> u64 {
    ms as u64 * SAMPLE_RATE as u64 / 1000
}

/// Marcos del mezclador a milisegundos (redondeo hacia abajo), para los registros.
pub fn frames_to_ms(frames: u64) -> u64 {
    frames * 1000 / SAMPLE_RATE as u64
}

/// Cómo empieza la canción que se carga.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Transition {
    /// Corte: lo de siempre (salto a mano, clic en otra canción, órdenes de Connect). Spotify
    /// tampoco funde al cambiar a mano. Si había un fundido, la saliente se suelta en 40 ms.
    #[default]
    Cut,
    /// Fundido aceptado por Spirc al acercarse el final natural de la canción.
    Crossfade,
    /// Salto automático durante un fundido (la entrante estaba oculta o no está disponible): la
    /// saliente sigue apagándose a su ritmo y la nueva entra con la rampa que le quede. Con un
    /// corte, los segundos que le faltaban a la canción anterior se perderían en 40 ms.
    AutoSkip,
}

/// Avance de un fundido, marco a marco. Las ganancias son de igual potencia (entrada = sen,
/// salida = cos del mismo ángulo): con dos canciones no correladas la potencia sumada no cambia
/// a lo largo del fundido, sin el bache de −3 dB a mitad de un fundido lineal.
///
/// Un marco lleva la misma ganancia en sus dos canales: la imagen estéreo no se mueve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ramp {
    done: u64,
    total: u64,
}

impl Ramp {
    /// Rampa de `total` marcos; con 0 ya está terminada.
    pub fn new(total: u64) -> Self {
        Self { done: 0, total }
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    /// Marcos que le faltan.
    pub fn remaining(&self) -> u64 {
        self.total - self.done
    }

    pub fn finished(&self) -> bool {
        self.done >= self.total
    }

    /// De 0 (primer marco) a 1 (terminada).
    pub fn progress(&self) -> f64 {
        if self.finished() {
            1.0
        } else {
            self.done as f64 / self.total as f64
        }
    }

    /// Ganancia del marco actual para la canción que entra: 0 exacto en el primero, 1 exacto al
    /// terminar (a partir de ahí la entrante vuelve a ser idéntica a sin fundido).
    pub fn gain_in(&self) -> f64 {
        if self.finished() {
            1.0
        } else {
            (self.progress() * FRAC_PI_2).sin()
        }
    }

    /// Ganancia del marco actual para la canción que sale: 1 exacto en el primero, 0 al terminar.
    pub fn gain_out(&self) -> f64 {
        if self.finished() {
            0.0
        } else {
            (self.progress() * FRAC_PI_2).cos()
        }
    }

    /// Pasa al marco siguiente; una rampa terminada se queda como está.
    pub fn advance(&mut self) {
        if self.done < self.total {
            self.done += 1;
        }
    }

    /// Aplica la entrada a un bloque de la canción que entra y devuelve si la rampa ha terminado.
    /// Lo que quede del bloque tras el final no se toca (ganancia 1 sin multiplicar).
    pub fn apply_in(&mut self, data: &mut [f64]) -> bool {
        for frame in data.chunks_exact_mut(CHANNELS) {
            if self.finished() {
                break;
            }
            let g = self.gain_in();
            for s in frame.iter_mut() {
                *s *= g;
            }
            self.advance();
        }
        self.finished()
    }
}

/// De dónde saca la saliente sus muestras: en el reproductor, su decodificador (junto con el
/// cargador del fichero y el id de la canción, que deben vivir lo mismo que él); en las pruebas,
/// paquetes fijos.
pub trait PacketSource {
    /// Siguiente paquete: muestras intercaladas a 44,1 kHz estéreo. `None` cuando se acaba (fin
    /// del fichero, error de decodificación o un paquete que no es PCM): ahí termina la saliente.
    fn next_samples(&mut self) -> Option<Vec<f64>>;
}

/// La canción que se va durante un fundido. Se mezcla sumándola, con su rampa de salida, bajo los
/// bloques de la entrante (o bajo silencio mientras la entrante carga).
pub struct Outgoing<S> {
    source: S,
    /// Su propio factor de normalización: cada canción suena a su nivel también durante el fundido.
    gain: f64,
    /// Muestras ya decodificadas y aún sin mezclar. Los paquetes de la saliente no coinciden con
    /// los bloques de la entrante: lo que sobra de uno espera aquí al siguiente bloque.
    pending: VecDeque<f64>,
    ramp: Ramp,
    /// Suelta de 40 ms si el fundido se corta; se multiplica por la rampa.
    release: Option<Ramp>,
    /// La fuente ya no da más.
    ended: bool,
}

impl<S: PacketSource> Outgoing<S> {
    /// La saliente con su ganancia y un fundido de `fade_frames` marcos que empieza ya.
    pub fn new(source: S, gain: f64, fade_frames: u64) -> Self {
        Self {
            source,
            gain,
            // Un paquete de Vorbis cabe de sobra: al mezclar no hace falta pedir memoria.
            pending: VecDeque::with_capacity(4096),
            ramp: Ramp::new(fade_frames),
            release: None,
            ended: false,
        }
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    pub fn ramp(&self) -> Ramp {
        self.ramp
    }

    /// La entrada que corresponde a la canción que entra ahora: lo que le queda a la salida, para
    /// que las dos rampas vayan alineadas (igual potencia), con un mínimo antichasquido por si la
    /// entrante llega tarde.
    pub fn incoming_ramp(&self) -> Ramp {
        Ramp::new(self.ramp.remaining().max(FADE_IN_MIN_FRAMES))
    }

    /// Ya no aporta nada: rampa o suelta completas, o la fuente se acabó. Lo que quedara del
    /// fichero ya iba a ganancia 0 y se descarta.
    pub fn finished(&self) -> bool {
        self.ended || self.ramp.finished() || self.release.is_some_and(|r| r.finished())
    }

    /// Cota de los marcos que aún puede aportar (sin contar un final de fichero antes de tiempo).
    pub fn frames_left(&self) -> u64 {
        if self.finished() {
            return 0;
        }
        let left = self.ramp.remaining();
        self.release.map_or(left, |r| left.min(r.remaining()))
    }

    /// Corta el fundido: la saliente se apaga en `RELEASE_FRAMES` en vez de en lo que le quedaba.
    /// Una suelta en curso no vuelve a empezar.
    pub fn release(&mut self) {
        if self.release.is_none() {
            self.release = Some(Ramp::new(RELEASE_FRAMES));
        }
    }

    pub fn is_releasing(&self) -> bool {
        self.release.is_some()
    }

    /// Suma la saliente a `data` (marcos completos, intercalados) y devuelve cuántos marcos ha
    /// aportado. Pide paquetes a la fuente a medida que los necesita, sean del tamaño que sean;
    /// tras el final de la fuente, o de la rampa, `data` queda como estaba.
    pub fn mix_into(&mut self, data: &mut [f64]) -> usize {
        let frames = data.len() / CHANNELS;
        let mut mixed = 0;
        while mixed < frames && !self.finished() {
            if self.pending.len() < CHANNELS && !self.refill() {
                break;
            }
            // Tramo hasta lo primero que se acabe: lo decodificado, el bloque, la rampa o la
            // suelta.
            let mut n = (self.pending.len() / CHANNELS)
                .min(frames - mixed)
                .min(usize::try_from(self.ramp.remaining()).unwrap_or(usize::MAX));
            if let Some(r) = &self.release {
                n = n.min(usize::try_from(r.remaining()).unwrap_or(usize::MAX));
            }
            let out = &mut data[mixed * CHANNELS..(mixed + n) * CHANNELS];
            let mut samples = self.pending.drain(..n * CHANNELS);
            for frame in out.chunks_exact_mut(CHANNELS) {
                let mut g = self.gain * self.ramp.gain_out();
                if let Some(r) = &mut self.release {
                    g *= r.gain_out();
                    r.advance();
                }
                self.ramp.advance();
                for s in frame.iter_mut() {
                    // `n` no pasa de lo que hay en `pending`: nunca falta una muestra.
                    *s += samples.next().unwrap_or(0.0) * g;
                }
            }
            mixed += n;
        }
        mixed
    }

    /// Rellena `pending` hasta tener al menos un marco. `false` si la fuente se acabó (lo que
    /// quedara, menos de un marco, se descarta).
    fn refill(&mut self) -> bool {
        let mut empty = 0;
        while self.pending.len() < CHANNELS {
            match self.source.next_samples() {
                Some(packet) if packet.is_empty() => {
                    empty += 1;
                    if empty >= MAX_EMPTY_PACKETS {
                        return self.end();
                    }
                }
                Some(packet) => self.pending.extend(packet),
                None => return self.end(),
            }
        }
        true
    }

    fn end(&mut self) -> bool {
        self.ended = true;
        self.pending.clear();
        false
    }
}

/// Cuánto fundir con lo que se sabe ahora, o `None` si no toca (todavía, o en absoluto).
///
/// El fundido dura lo pedido, con tope en 12 s y en media duración de cada canción (una canción
/// corta no puede pasarse la mitad sonando a medias). Se pide cuando a la actual le quedan como
/// mucho ese tiempo más `CROSSFADE_LEAD_MS`; si ya queda menos (la siguiente estuvo lista tarde,
/// o se buscó cerca del final) se funde lo que quede. Por debajo de `CROSSFADE_MIN_MS`, nada.
pub fn plan_fade_ms(
    cfg_ms: u32,
    remaining_ms: u32,
    cur_dur_ms: u32,
    next_dur_ms: u32,
) -> Option<u32> {
    let f = cfg_ms
        .min(CROSSFADE_MAX_MS)
        .min(cur_dur_ms / 2)
        .min(next_dur_ms / 2);
    if f < CROSSFADE_MIN_MS || remaining_ms > f.saturating_add(CROSSFADE_LEAD_MS) {
        return None;
    }
    let fade = f.min(remaining_ms);
    (fade >= CROSSFADE_MIN_MS).then_some(fade)
}

/// Lo que hace falta de una canción para la regla del álbum.
struct AlbumSlot<'a> {
    album: &'a str,
    artists: &'a [String],
    disc: u32,
    number: u32,
}

fn album_slot(fields: &UniqueFields) -> Option<AlbumSlot<'_>> {
    match fields {
        UniqueFields::Track {
            album,
            album_artists,
            number,
            disc_number,
            ..
        } => Some(AlbumSlot {
            album,
            artists: album_artists,
            disc: *disc_number,
            number: *number,
        }),
        // Un archivo local solo cuenta si trae todas las etiquetas.
        UniqueFields::Local {
            album: Some(album),
            album_artists: Some(album_artists),
            number: Some(number),
            disc_number: Some(disc_number),
            ..
        } => Some(AlbumSlot {
            album,
            artists: std::slice::from_ref(album_artists),
            disc: *disc_number,
            number: *number,
        }),
        _ => None,
    }
}

/// `b` sigue a `a` dentro del mismo álbum: mismo nombre de álbum y mismos artistas del álbum, y
/// la pista siguiente del mismo disco o la primera del disco siguiente. Spotify no funde esas
/// transiciones para respetar las que hizo el artista (temas que se encadenan). Es una
/// aproximación: los metadatos no traen el id del álbum, y Spirc completa la regla con el
/// contexto (reproduciendo un álbum sin aleatorio).
pub fn is_album_continuation(a: &UniqueFields, b: &UniqueFields) -> bool {
    if std::mem::discriminant(a) != std::mem::discriminant(b) {
        return false;
    }
    let (Some(a), Some(b)) = (album_slot(a), album_slot(b)) else {
        return false;
    };
    // Sin nombre de álbum o sin número de pista no hay con qué decidir: se funde.
    if a.album.is_empty() || a.number == 0 || a.album != b.album || a.artists != b.artists {
        return false;
    }
    let next_in_disc = b.disc == a.disc && a.number.checked_add(1) == Some(b.number);
    let next_disc = a.disc.checked_add(1) == Some(b.disc) && b.number == 1;
    next_in_disc || next_disc
}

/// Se está escuchando un álbum en orden: el contexto es un álbum, sin aleatorio, y la canción
/// actual y la siguiente salen de él (no de la cola ni de autoplay). Es el caso que Spotify
/// documenta para no fundir; a diferencia de `is_album_continuation` no depende de metadatos (ni
/// confunde dos álbumes del mismo nombre) y también vale en aleatorio apagado con pistas sin
/// número.
pub fn is_album_in_order(
    context_uri: &str,
    shuffling: bool,
    current_from_context: bool,
    next_from_context: bool,
) -> bool {
    context_uri.starts_with("spotify:album:")
        && !shuffling
        && current_from_context
        && next_from_context
}

/// Margen sobre `fundido + CROSSFADE_LEAD_MS` con el que Spirc aún da por buena una propuesta
/// según su propia cuenta de la posición. Esa cuenta puede ir hasta un segundo por detrás de lo
/// decodificado (lo que guarda la salida) y el reproductor solo la corrige con un segundo de
/// retraso. Más allá la propuesta es vieja: se buscó hacia atrás mientras viajaba, y aceptarla
/// saltaría a la siguiente a mitad de canción.
pub const OFFER_POSITION_SLACK_MS: u32 = 2_500;

/// Lo que Spirc sabe cuando el reproductor propone un fundido (`PlayerEvent::CrossfadeReady`).
/// Solo datos: la decisión (`decide_offer`) se prueba aquí, sin Spirc ni sesión.
#[derive(Clone, Copy, Debug)]
pub struct FadeOffer {
    /// Spirc da la canción por sonando (no en pausa ni cargando).
    pub playing: bool,
    /// «Repetir canción»: la misma vuelve a empezar sin hueco, nunca fundida consigo misma.
    pub repeat_track: bool,
    /// Participante de una Jam: manda el anfitrión, y avanzar por nuestra cuenta podría cruzarse
    /// con su siguiente actualización de la cola compartida. Es una decisión prudente.
    pub jam_participant: bool,
    /// Ya se aceptó un fundido para esta canción (la misma propuesta, repetida).
    pub already_accepted: bool,
    /// La canción actual y la siguiente de la cola de Spirc son las de la propuesta: la cola, el
    /// aleatorio o la canción pudieron cambiar mientras viajaba.
    pub queue_matches: bool,
    /// Lo que le queda a la canción según Spirc; `None` si no conoce la duración.
    pub remaining_ms: Option<u32>,
    /// Duración prevista del fundido.
    pub fade_ms: u32,
    /// Un álbum escuchado en orden (`is_album_in_order`).
    pub album_in_order: bool,
    /// Por los metadatos, la siguiente es la pista que sigue del mismo álbum (p. ej. una playlist
    /// ordenada como el álbum; `is_album_continuation`).
    pub album_continuation: bool,
    /// «Fundir también canciones seguidas de un mismo álbum».
    pub crossfade_albums: bool,
}

/// Por qué Spirc no acepta un fundido. Entonces no pasa nada más: la canción suena hasta el final
/// y la siguiente entra sin hueco, como sin fundido.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FadeDecline {
    NotPlaying,
    RepeatTrack,
    Jam,
    AlreadyAccepted,
    QueueChanged,
    NotNearEnd,
    SameAlbum,
}

impl FadeDecline {
    /// Motivo para el registro (`[fundido] … no se funde (motivo)`).
    pub fn reason(self) -> &'static str {
        match self {
            Self::NotPlaying => "no está sonando",
            Self::RepeatTrack => "repetir canción",
            Self::Jam => "participante de una Jam",
            Self::AlreadyAccepted => "ya aceptado",
            Self::QueueChanged => "la cola cambió",
            Self::NotNearEnd => "ya no está cerca del final",
            Self::SameAlbum => "sigue el mismo álbum",
        }
    }
}

/// Decide en Spirc si se funde lo que propone el reproductor. Spirc sabe lo que el reproductor
/// no: el contexto, la repetición, la Jam y cómo está la cola ahora. El fundido solo cabe en una
/// transición natural; ante la duda no se funde (queda lo de siempre, sin hueco, que nunca suena
/// mal).
pub fn decide_offer(offer: &FadeOffer) -> Result<(), FadeDecline> {
    if !offer.playing {
        return Err(FadeDecline::NotPlaying);
    }
    if offer.repeat_track {
        return Err(FadeDecline::RepeatTrack);
    }
    if offer.jam_participant {
        return Err(FadeDecline::Jam);
    }
    if offer.already_accepted {
        return Err(FadeDecline::AlreadyAccepted);
    }
    if !offer.queue_matches {
        return Err(FadeDecline::QueueChanged);
    }
    let window_ms = offer
        .fade_ms
        .saturating_add(CROSSFADE_LEAD_MS)
        .saturating_add(OFFER_POSITION_SLACK_MS);
    if offer.remaining_ms.is_some_and(|left| left > window_ms) {
        return Err(FadeDecline::NotNearEnd);
    }
    // Regla de Spotify: dentro de un álbum las transiciones son las del artista (temas que se
    // encadenan), salvo que se pida fundir también ahí.
    if !offer.crossfade_albums && (offer.album_in_order || offer.album_continuation) {
        return Err(FadeDecline::SameAlbum);
    }
    Ok(())
}

/// Estado y parámetros del limitador de la mezcla. Es el mismo limitador por anticipación del
/// nivel «Alto» (Giannoulis, Massberg y Reiss, 2012: rodilla suave en dB, detector de pico con
/// ataque y liberación, un solo factor para los dos canales), pero con su propio estado: solo
/// actúa mientras suenan dos canciones, y cada fundido empieza con él a cero (`reset`) para no
/// arrastrar la reducción de otro momento.
#[derive(Clone, Debug)]
pub struct LimiterState {
    threshold_db: f64,
    knee_db: f64,
    knee_factor: f64,
    attack_cf: f64,
    release_cf: f64,
    integrators: [f64; CHANNELS],
    peaks: [f64; CHANNELS],
}

impl LimiterState {
    /// `attack_cf`/`release_cf` son coeficientes por muestra intercalada, como los de
    /// `PlayerConfig` (`duration_to_coefficient`).
    pub fn new(threshold_db: f64, knee_db: f64, attack_cf: f64, release_cf: f64) -> Self {
        Self {
            threshold_db,
            knee_db,
            // Con rodilla 0 (dura) el término de la rodilla nunca se usa salvo justo en el umbral,
            // donde vale 0: sin esto sería 0·∞ = NaN.
            knee_factor: if knee_db > 0.0 { 1.0 / (8.0 * knee_db) } else { 0.0 },
            attack_cf,
            release_cf,
            integrators: [0.0; CHANNELS],
            peaks: [0.0; CHANNELS],
        }
    }

    /// Vuelve a cero los integradores: lo que se hace al empezar cada fundido.
    pub fn reset(&mut self) {
        self.integrators = [0.0; CHANNELS];
        self.peaks = [0.0; CHANNELS];
    }

    /// Reducción que aplica ahora mismo, en dB (0 si no actúa).
    pub fn reduction_db(&self) -> f64 {
        f64::max(self.peaks[0], self.peaks[1])
    }

    /// Aún reduce de forma apreciable (ver `LIMITER_SETTLED_DB`).
    pub fn is_engaged(&self) -> bool {
        self.reduction_db() > LIMITER_SETTLED_DB
    }
}

/// Limita la mezcla (bloque de marcos completos) para que la suma de las dos canciones no pase de
/// `threshold_db` más que en el borde de un golpe (lo que deja pasar el ataque lo recoge
/// `safety_clamp`). Donde no actúa, la salida es exactamente la entrada: no se toca la muestra ni
/// para el desplazamiento que evita −∞ dB en el silencio.
///
/// A diferencia del bucle del nivel «Alto», que reduce cada muestra con lo que llevaba visto hasta
/// ella, aquí se miran primero los dos canales del marco y se aplica la misma reducción a ambos:
/// la imagen estéreo no se mueve ni una muestra.
pub fn mix_limit(data: &mut [f64], state: &mut LimiterState) {
    for frame in data.chunks_mut(CHANNELS) {
        for (ch, sample) in frame.iter().enumerate() {
            // Pasos 1-4: rectificación, paso a dB y cálculo de la reducción con rodilla suave.
            // Una muestra no finita no entra en el detector (lo dejaría bloqueado en ±∞ o NaN
            // el resto del fundido); de ella se encarga `safety_clamp`.
            let level = if sample.is_finite() { sample.abs() } else { 0.0 };
            let bias_db = 20.0 * (level + f64::MIN_POSITIVE).log10() - state.threshold_db;
            let knee_boundary_db = bias_db * 2.0;
            let limiter_db = if knee_boundary_db < -state.knee_db {
                0.0
            } else if knee_boundary_db.abs() <= state.knee_db {
                let term = knee_boundary_db + state.knee_db;
                term * term * state.knee_factor
            } else {
                bias_db
            };

            // Paso 5: detector de pico suave y desacoplado por canal.
            let integrator = &mut state.integrators[ch];
            *integrator = f64::max(
                limiter_db,
                state.release_cf * *integrator + (1.0 - state.release_cf) * limiter_db,
            );
            let peak = &mut state.peaks[ch];
            *peak = state.attack_cf * *peak + (1.0 - state.attack_cf) * *integrator;
        }

        // Pasos 6-8: el mayor de los dos canales manda.
        let reduction_db = state.reduction_db();
        if reduction_db > 0.0 {
            let g = 10f64.powf(-reduction_db / 20.0);
            for sample in frame.iter_mut() {
                *sample *= g;
            }
        }
    }
}

/// Suelta del limitador de la mezcla cuando ya suena una sola canción: la reducción que llevaba
/// vuelve a 0 con su propia liberación (el detector ve silencio), en vez de desaparecer de golpe
/// en el primer bloque sin suma. Con un máster cerca de 0 dBFS al final del fundido, ese salto
/// (varias décimas de dB de un marco al siguiente) se oye como un chasquido. Aquí ya no se limita
/// nada nuevo: la canción sola sale como sin fundido. Al acabar, el estado queda a cero.
pub fn release_limit(data: &mut [f64], state: &mut LimiterState) {
    for frame in data.chunks_mut(CHANNELS) {
        for ch in 0..CHANNELS {
            // Paso 5 con una entrada de 0 dB de reducción: solo liberación.
            let integrator = &mut state.integrators[ch];
            *integrator *= state.release_cf;
            let peak = &mut state.peaks[ch];
            *peak = state.attack_cf * *peak + (1.0 - state.attack_cf) * *integrator;
        }
        let reduction_db = state.reduction_db();
        if reduction_db > 0.0 {
            let g = 10f64.powf(-reduction_db / 20.0);
            for sample in frame.iter_mut() {
                *sample *= g;
            }
        }
    }
    if !state.is_engaged() {
        state.reset();
    }
}

/// Último recurso contra el recorte mientras se mezcla, después del limitador. Idéntica a la
/// entrada hasta `SAFETY_CLAMP_KNEE` (0,98); por encima satura suave (tanh, sin escalón de
/// pendiente) hacia 1,0 sin pasarlo nunca. Un NaN sale como silencio.
#[inline]
pub fn safety_clamp(x: f64) -> f64 {
    const T: f64 = SAFETY_CLAMP_KNEE;
    let a = x.abs();
    if a <= T {
        return x;
    }
    if a.is_nan() {
        return 0.0;
    }
    (T + (1.0 - T) * ((a - T) / (1.0 - T)).tanh()).copysign(x)
}

/// `safety_clamp` sobre un bloque entero.
pub fn clamp_block(data: &mut [f64]) {
    for s in data.iter_mut() {
        *s = safety_clamp(*s);
    }
}

/// Qué pasó con el fundido al empezar a sonar una canción (`Crossfader::start_incoming`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncomingStart {
    /// No había fundido: la canción empieza como siempre.
    Plain,
    /// Es la canción esperada: entra con esta rampa mientras la saliente se apaga.
    Fading(Ramp),
    /// Había una saliente, pero esta canción no puede fundirse con ella (empieza en pausa, o no es
    /// la esperada): se quitó sin más, para que no suene encima de nada más tarde.
    Dropped,
}

/// El fundido en curso: la canción que se va, la rampa de entrada de la que llega y la canción a
/// la que espera la saliente. `player.rs` lo consulta en cada paso (cargar, empezar a sonar, cada
/// paquete, cortar); aquí vive todo lo que decide qué se oye, para probarlo con decodificadores de
/// prueba sin sesión ni salida de audio.
///
/// Reglas que el reproductor sigue con él:
/// - mientras no está activo (`is_active`), el reproductor no lo usa: lo que sale es idéntico, bit
///   a bit, a no tener fundido;
/// - una saliente solo existe mientras alguien la escribe: mezclada bajo la canción que suena
///   (`process`), sola mientras la siguiente carga (`tail`) o retenida en pausa; en cualquier otro
///   caso se suelta en el acto (`cancel`) o se quita (`clear`), nunca se deja para luego.
pub struct Crossfader<S, T> {
    outgoing: Option<Outgoing<S>>,
    fade_in: Option<Ramp>,
    /// La canción con la que se funde la saliente: solo esa entra con rampa.
    target: Option<T>,
    limiter: LimiterState,
}

impl<S: PacketSource, T: PartialEq> Crossfader<S, T> {
    pub fn new(limiter: LimiterState) -> Self {
        Self {
            outgoing: None,
            fade_in: None,
            target: None,
            limiter,
        }
    }

    /// Hay algo del fundido que aplicar a lo que suena: saliente, rampa de entrada o la suelta del
    /// limitador de la mezcla (`settle`).
    pub fn is_active(&self) -> bool {
        self.is_fading() || self.limiter.is_engaged()
    }

    /// Hay un fundido en curso (saliente o rampa de entrada). La suelta del limitador no cuenta:
    /// no impide proponer ni empezar el siguiente.
    pub fn is_fading(&self) -> bool {
        self.outgoing.is_some() || self.fade_in.is_some()
    }

    pub fn has_outgoing(&self) -> bool {
        self.outgoing.is_some()
    }

    pub fn outgoing(&self) -> Option<&Outgoing<S>> {
        self.outgoing.as_ref()
    }

    pub fn fade_in(&self) -> Option<Ramp> {
        self.fade_in
    }

    /// La saliente espera a `id`: si empieza a sonar ahora, entra fundiéndose.
    pub fn expects(&self, id: &T) -> bool {
        self.outgoing.as_ref().is_some_and(|o| !o.finished()) && self.target.as_ref() == Some(id)
    }

    /// Empieza un fundido de `fade_frames` marcos: `source` (la canción que sonaba, con su
    /// ganancia) pasa a ser la saliente y espera a `target`. El limitador de la mezcla empieza a
    /// cero (`limiter` recién creado con los tiempos de ahora). Lo que hubiera antes se descarta:
    /// el reproductor ya lo ha cortado.
    pub fn begin(&mut self, source: S, gain: f64, fade_frames: u64, target: T, mut limiter: LimiterState) {
        self.outgoing = Some(Outgoing::new(source, gain, fade_frames));
        self.fade_in = None;
        self.target = Some(target);
        // Si el anterior aún se estaba soltando (canciones muy cortas), su reducción sigue desde
        // donde iba: ponerla a cero de golpe sería el mismo salto que `settle` evita.
        if self.limiter.is_engaged() {
            limiter.integrators = self.limiter.integrators;
            limiter.peaks = self.limiter.peaks;
        }
        self.limiter = limiter;
    }

    /// La canción `id` empieza a sonar (`playing`) o queda lista en pausa. Si es la que espera la
    /// saliente y va a sonar, entra con lo que le queda a la rampa de salida (con el mínimo
    /// antichasquido). Si no, la saliente se quita: en pausa nadie la escribiría y otra canción no
    /// debe heredarla.
    pub fn start_incoming(&mut self, id: &T, playing: bool) -> IncomingStart {
        let target = self.target.take();
        let alive = self.outgoing.as_ref().filter(|o| !o.finished());
        match alive {
            Some(o) if playing && target.as_ref() == Some(id) => {
                let ramp = o.incoming_ramp();
                self.fade_in = Some(ramp);
                IncomingStart::Fading(ramp)
            }
            _ => {
                self.fade_in = None;
                if self.outgoing.take().is_some() {
                    IncomingStart::Dropped
                } else {
                    IncomingStart::Plain
                }
            }
        }
    }

    /// Salto automático durante el fundido (la canción que entraba estaba oculta o no se pudo
    /// cargar): la saliente sigue apagándose a su ritmo y espera a `next`, que entrará con lo que
    /// le quede a la rampa. La entrante descartada deja de sonar (lo hace el reproductor al
    /// cambiar de estado). `false` si no hay fundido que conservar.
    pub fn auto_skip(&mut self, next: T) -> bool {
        match &self.outgoing {
            Some(o) if !o.finished() && !o.is_releasing() => {
                self.fade_in = None;
                self.target = Some(next);
                true
            }
            _ => false,
        }
    }

    /// Un bloque de la canción que suena, ya con su propia ganancia: le aplica la rampa de entrada
    /// y le suma la saliente. Devuelve si la saliente sonaba en este bloque (entonces la suma hay
    /// que protegerla con `protect`). Lo que termina se quita solo.
    pub fn process(&mut self, data: &mut [f64]) -> bool {
        if let Some(r) = &mut self.fade_in {
            if r.apply_in(data) {
                self.fade_in = None;
            }
        }
        let Some(o) = &mut self.outgoing else {
            return false;
        };
        o.mix_into(data);
        if o.finished() {
            self.outgoing = None;
        }
        true
    }

    /// Protección de un bloque en el que suenan dos canciones: el limitador de la mezcla (salvo
    /// que la normalización ya lleve el suyo, `own_limiter`) y la saturación final.
    pub fn protect(&mut self, data: &mut [f64], own_limiter: bool) {
        if !own_limiter {
            mix_limit(data, &mut self.limiter);
        }
        clamp_block(data);
    }

    /// Un bloque en el que ya suena una sola canción (la saliente acabó o no la había): si el
    /// limitador de la mezcla quedó reduciendo, se suelta poco a poco (`release_limit`). Si no,
    /// no toca nada.
    pub fn settle(&mut self, data: &mut [f64]) {
        if self.limiter.is_engaged() {
            release_limit(data, &mut self.limiter);
        }
    }

    /// Siguiente bloque de la saliente sola (como mucho `TAIL_FRAMES`), para cuando la siguiente
    /// aún carga o para la suelta. `None` cuando ya no aporta nada; entonces se quita.
    pub fn tail(&mut self) -> Option<Vec<f64>> {
        let o = self.outgoing.as_mut()?;
        let frames = usize::try_from(o.frames_left().min(TAIL_FRAMES as u64)).unwrap_or(0);
        let mut block = vec![0.0; frames * CHANNELS];
        let mixed = if frames > 0 { o.mix_into(&mut block) } else { 0 };
        if o.finished() {
            self.outgoing = None;
        }
        if mixed == 0 {
            self.outgoing = None;
            return None;
        }
        block.truncate(mixed * CHANNELS);
        Some(block)
    }

    /// Corta el fundido (otra canción, búsqueda, salto a mano). Con la salida sonando
    /// (`audible`), la saliente se suelta en 40 ms y se devuelven esos bloques para escribirlos
    /// ya, antes de seguir: nada queda pendiente de mezclarse con lo que suene después. Sin salida
    /// sonando no se oiría: se quita sin más. La rampa de entrada también se acaba (la canción que
    /// entraba sigue, si sigue, a su volumen).
    pub fn cancel(&mut self, audible: bool) -> Vec<Vec<f64>> {
        self.fade_in = None;
        self.target = None;
        let mut blocks = Vec::new();
        if audible {
            if let Some(o) = &mut self.outgoing {
                o.release();
            }
            while blocks.len() < MAX_RELEASE_BLOCKS {
                match self.tail() {
                    Some(block) => blocks.push(block),
                    None => break,
                }
            }
        }
        self.outgoing = None;
        // El limitador de la mezcla no se pone a cero: si reducía, lo que suene después se suelta
        // con `settle` (sin escalón) y enseguida vuelve a ser bit a bit lo de sin fundido.
        blocks
    }

    /// Lo quita todo sin escribir nada. Devuelve si había una saliente. Lo siguiente que suene
    /// llega tras un silencio (parar, pausa, salida caída): el limitador empieza a cero.
    pub fn clear(&mut self) -> bool {
        self.fade_in = None;
        self.target = None;
        self.limiter.reset();
        self.outgoing.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_metadata::artist::ArtistsWithRole;
    use std::f64::consts::PI;
    use std::path::PathBuf;

    // ---------- utilidades ----------

    /// Fuente de prueba: devuelve los paquetes dados y luego se acaba.
    struct Packets(VecDeque<Vec<f64>>);

    impl Packets {
        fn new(packets: Vec<Vec<f64>>) -> Self {
            Self(packets.into())
        }
    }

    impl PacketSource for Packets {
        fn next_samples(&mut self) -> Option<Vec<f64>> {
            self.0.pop_front()
        }
    }

    /// Fuente infinita de un valor constante, en paquetes de `size` muestras.
    struct Constant {
        value: f64,
        size: usize,
    }

    impl PacketSource for Constant {
        fn next_samples(&mut self) -> Option<Vec<f64>> {
            Some(vec![self.value; self.size])
        }
    }

    /// Una muestra intercalada ya preparada, troceada en paquetes de tamaños que van rotando.
    struct Chunked {
        data: Vec<f64>,
        pos: usize,
        sizes: Vec<usize>,
        turn: usize,
    }

    impl PacketSource for Chunked {
        fn next_samples(&mut self) -> Option<Vec<f64>> {
            if self.pos >= self.data.len() {
                return None;
            }
            let size = self.sizes[self.turn % self.sizes.len()];
            self.turn += 1;
            let end = (self.pos + size).min(self.data.len());
            let out = self.data[self.pos..end].to_vec();
            self.pos = end;
            Some(out)
        }
    }

    /// xorshift64*: ruido determinista sin dependencias.
    struct Rng(u64);

    impl Rng {
        /// Uniforme en [−1, 1).
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
            (v >> 11) as f64 / (1u64 << 52) as f64 - 1.0
        }
    }

    /// Ruido rosa (filtro de Paul Kellett) de `frames` marcos, canales independientes.
    fn pink(seed: u64, frames: usize) -> Vec<f64> {
        let mut rng = Rng(seed);
        let mut state = [[0.0f64; 7]; CHANNELS];
        let mut out = Vec::with_capacity(frames * CHANNELS);
        for _ in 0..frames {
            for b in state.iter_mut() {
                let w = rng.next();
                b[0] = 0.99886 * b[0] + w * 0.0555179;
                b[1] = 0.99332 * b[1] + w * 0.0750759;
                b[2] = 0.96900 * b[2] + w * 0.1538520;
                b[3] = 0.86650 * b[3] + w * 0.3104856;
                b[4] = 0.55000 * b[4] + w * 0.5329522;
                b[5] = -0.7616 * b[5] - w * 0.0168980;
                out.push(b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362);
                b[6] = w * 0.115926;
            }
        }
        out
    }

    fn power(v: &[f64]) -> f64 {
        v.iter().map(|x| x * x).sum::<f64>() / v.len() as f64
    }

    fn dot(a: &[f64], b: &[f64]) -> f64 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    fn scale_to_rms(v: &mut [f64], rms: f64) {
        let k = rms / power(v).sqrt();
        v.iter_mut().for_each(|x| *x *= k);
    }

    fn coefficient(ms: f64) -> f64 {
        // Igual que `duration_to_coefficient` del reproductor: por muestra intercalada.
        (-1.0 / (ms / 1000.0 * (SAMPLE_RATE as f64 * CHANNELS as f64))).exp()
    }

    /// El limitador de la mezcla con los tiempos del nivel «Alto» de Nanofy (5 ms, 100 ms,
    /// rodilla de 1 dB).
    fn limiter() -> LimiterState {
        LimiterState::new(MIX_LIMIT_THRESHOLD_DBFS, 1.0, coefficient(5.0), coefficient(100.0))
    }

    fn db(ratio: f64) -> f64 {
        10.0 * ratio.log10()
    }

    // ---------- rampa ----------

    #[test]
    fn ramp_endpoints_and_equal_power() {
        let n = 441;
        let mut r = Ramp::new(n);
        assert_eq!(r.gain_in(), 0.0);
        assert_eq!(r.gain_out(), 1.0);
        let (mut last_in, mut last_out) = (-1.0, 2.0);
        for _ in 0..n {
            let (gi, go) = (r.gain_in(), r.gain_out());
            assert!((gi * gi + go * go - 1.0).abs() < 1e-12, "in²+out² = {}", gi * gi + go * go);
            assert!(gi > last_in && go < last_out, "las ganancias no son monótonas");
            (last_in, last_out) = (gi, go);
            r.advance();
        }
        assert!(r.finished());
        assert_eq!(r.remaining(), 0);
        assert_eq!(r.gain_in(), 1.0);
        assert_eq!(r.gain_out(), 0.0);
        // Terminada se queda terminada.
        r.advance();
        assert_eq!(r.remaining(), 0);
        assert_eq!(r.progress(), 1.0);
    }

    #[test]
    fn ramp_of_zero_frames_is_already_done() {
        let r = Ramp::new(0);
        assert!(r.finished());
        assert_eq!((r.gain_in(), r.gain_out(), r.progress()), (1.0, 0.0, 1.0));
    }

    #[test]
    fn ramp_midpoint_is_minus_3_db_each() {
        let mut r = Ramp::new(1000);
        for _ in 0..500 {
            r.advance();
        }
        assert!((r.gain_in() - 0.5f64.sqrt()).abs() < 1e-12);
        assert!((20.0 * r.gain_out().log10() + 3.0103).abs() < 1e-3);
    }

    #[test]
    fn apply_in_spans_blocks_and_leaves_the_rest_untouched() {
        let mut r = Ramp::new(5);
        let mut reference = Ramp::new(5);
        let mut a = vec![1.0; 6]; // 3 marcos
        let mut b = vec![1.0; 8]; // 4 marcos: 2 de rampa y 2 intactos
        assert!(!r.apply_in(&mut a));
        assert!(r.apply_in(&mut b));
        let all: Vec<f64> = a.iter().chain(&b).copied().collect();
        for (f, frame) in all.chunks(CHANNELS).enumerate() {
            let want = if f < 5 { reference.gain_in() } else { 1.0 };
            reference.advance();
            assert_eq!(frame, [want, want], "marco {f}");
        }
        assert_eq!(all[0], 0.0);
    }

    // ---------- saliente ----------

    #[test]
    fn mix_into_spans_packets_of_6_2_and_10_and_pads_after_eof() {
        let src: Vec<f64> = (1..=18).map(f64::from).collect();
        let packets = vec![src[..6].to_vec(), src[6..8].to_vec(), src[8..].to_vec()];
        let mut out = Outgoing::new(Packets::new(packets), 0.5, 100);
        let mut reference = Ramp::new(100);
        let mut expected = Vec::new();
        for frame in src.chunks(CHANNELS) {
            let g = 0.5 * reference.gain_out();
            reference.advance();
            expected.extend(frame.iter().map(|s| s * g));
        }

        // 4 marcos: el primer paquete entero (3) y el segundo (1).
        let mut a = vec![0.0; 8];
        assert_eq!(out.mix_into(&mut a), 4);
        assert_eq!(a, expected[..8]);
        assert!(!out.finished());

        // 6 marcos sobre algo que ya sonaba: suma (no sustituye) los 5 del último paquete, y el
        // sexto, tras el final, queda como estaba.
        let mut b = vec![7.0; 12];
        assert_eq!(out.mix_into(&mut b), 5);
        for i in 0..10 {
            assert_eq!(b[i], 7.0 + expected[8 + i], "muestra {i}");
        }
        assert_eq!(b[10..], [7.0, 7.0]);
        assert!(out.finished());
        assert_eq!(out.ramp().remaining(), 100 - 9);

        // Acabada no aporta nada más.
        let mut c = vec![0.25; 4];
        assert_eq!(out.mix_into(&mut c), 0);
        assert_eq!(c, [0.25; 4]);
        assert_eq!(out.frames_left(), 0);
    }

    #[test]
    fn mix_into_handles_packets_that_split_a_frame() {
        // Paquetes de 3, 5 y 3 muestras: los marcos quedan partidos entre paquetes y la última
        // muestra suelta (sin su pareja) se descarta.
        let src: Vec<f64> = (1..=11).map(f64::from).collect();
        let packets = vec![src[..3].to_vec(), src[3..8].to_vec(), src[8..].to_vec()];
        let mut out = Outgoing::new(Packets::new(packets), 1.0, 1_000_000);
        let mut reference = Ramp::new(1_000_000);
        let mut data = vec![0.0; 16];
        assert_eq!(out.mix_into(&mut data), 5);
        for (f, frame) in data[..10].chunks(CHANNELS).enumerate() {
            let g = reference.gain_out();
            reference.advance();
            assert_eq!(frame, [src[2 * f] * g, src[2 * f + 1] * g], "marco {f}");
        }
        assert_eq!(data[10..], [0.0; 6]);
        assert!(out.finished());
    }

    #[test]
    fn ramp_end_stops_before_eof_and_discards_the_rest() {
        let mut out = Outgoing::new(Constant { value: 1.0, size: 6 }, 1.0, 10);
        assert_eq!(out.frames_left(), 10);
        let mut data = vec![0.0; 40];
        assert_eq!(out.mix_into(&mut data), 10);
        assert!(data[..20].iter().all(|&s| s > 0.0));
        assert_eq!(data[20..], [0.0; 20]);
        assert!(out.finished());
    }

    #[test]
    fn release_reaches_zero_in_1764_frames() {
        let fade = ms_to_frames(6000);
        let mut out = Outgoing::new(Constant { value: 1.0, size: 2048 }, 1.0, fade);
        let mut warm = vec![0.0; 200];
        assert_eq!(out.mix_into(&mut warm), 100);

        out.release();
        assert!(out.is_releasing());
        assert_eq!(out.frames_left(), RELEASE_FRAMES);
        let mut data = vec![0.0; 2000 * CHANNELS];
        assert_eq!(out.mix_into(&mut data), RELEASE_FRAMES as usize);
        assert!(out.finished());

        let mut ramp = Ramp::new(fade);
        for _ in 0..100 {
            ramp.advance();
        }
        let mut rel = Ramp::new(RELEASE_FRAMES);
        let mut last = f64::INFINITY;
        for (f, frame) in data.chunks(CHANNELS).enumerate() {
            let want = if f < RELEASE_FRAMES as usize {
                ramp.gain_out() * rel.gain_out()
            } else {
                0.0
            };
            ramp.advance();
            rel.advance();
            assert!((frame[0] - want).abs() < 1e-15 && frame[0] == frame[1], "marco {f}");
            assert!(frame[0] <= last, "la suelta sube en el marco {f}");
            last = frame[0];
        }
        // Empieza donde estaba (sin escalón) y su último marco ya casi no suena.
        assert!((data[0] - ramp_gain_after(fade, 100)).abs() < 1e-15);
        assert!(data[(RELEASE_FRAMES as usize - 1) * CHANNELS] < 1e-3);
    }

    fn ramp_gain_after(total: u64, done: u64) -> f64 {
        let mut r = Ramp::new(total);
        for _ in 0..done {
            r.advance();
        }
        r.gain_out()
    }

    #[test]
    fn release_is_written_in_two_tail_blocks_and_does_not_restart() {
        let mut out = Outgoing::new(Constant { value: 0.5, size: 1000 }, 1.0, ms_to_frames(6000));
        out.release();
        let mut tail = vec![0.0; TAIL_FRAMES * CHANNELS];
        out.mix_into(&mut tail);
        assert_eq!(out.frames_left(), RELEASE_FRAMES - TAIL_FRAMES as u64);
        // Pedirla otra vez no la alarga.
        out.release();
        assert_eq!(out.frames_left(), RELEASE_FRAMES - TAIL_FRAMES as u64);
        let mut writes = 1;
        while !out.finished() {
            tail.fill(0.0);
            out.mix_into(&mut tail);
            writes += 1;
        }
        assert_eq!(writes, 2);
    }

    #[test]
    fn release_shorter_than_remaining_ramp_only() {
        // Si a la rampa le quedan menos de 40 ms, manda la rampa.
        let mut out = Outgoing::new(Constant { value: 1.0, size: 64 }, 1.0, 500);
        out.release();
        assert_eq!(out.frames_left(), 500);
        let mut data = vec![0.0; 4000];
        assert_eq!(out.mix_into(&mut data), 500);
    }

    #[test]
    fn endless_empty_packets_end_the_outgoing() {
        let mut out = Outgoing::new(Constant { value: 1.0, size: 0 }, 1.0, 1000);
        let mut data = vec![0.0; 64];
        assert_eq!(out.mix_into(&mut data), 0);
        assert!(out.finished());
        assert_eq!(data, [0.0; 64]);
    }

    #[test]
    fn incoming_ramp_follows_the_outgoing_with_an_anti_click_minimum() {
        let mut out = Outgoing::new(Constant { value: 1.0, size: 64 }, 1.0, 10_000);
        assert_eq!(out.incoming_ramp(), Ramp::new(10_000));
        let mut data = vec![0.0; 9_000 * CHANNELS];
        out.mix_into(&mut data);
        assert_eq!(out.incoming_ramp(), Ramp::new(FADE_IN_MIN_FRAMES));
    }

    #[test]
    fn pending_does_not_grow() {
        // Bloques de 1500 marcos contra paquetes de 2048 muestras: lo pendiente nunca pasa de un
        // paquete, y la cola no vuelve a pedir memoria tras el primero.
        let mut out = Outgoing::new(Constant { value: 0.1, size: 2048 }, 1.0, u64::MAX);
        let mut data = vec![0.0; 1500 * CHANNELS];
        out.mix_into(&mut data);
        let cap = out.pending.capacity();
        for _ in 0..200 {
            out.mix_into(&mut data);
            assert!(out.pending.len() < 2048);
        }
        assert_eq!(out.pending.capacity(), cap);
    }

    // ---------- mezcla completa ----------

    /// Fundido de `fade_ms` entre dos fuentes ya preparadas, por el mismo camino que el
    /// reproductor (`Crossfader`): rampa de entrada sobre la entrante, saliente sumada, limitador y
    /// protección final. Los bloques de la entrante (1500 marcos) no coinciden con los paquetes de
    /// la saliente.
    fn crossfade(outgoing: Vec<f64>, incoming: &[f64], fade_ms: u32) -> Vec<f64> {
        let sizes = vec![1100, 300, 4096, 2, 2048];
        let source = Chunked { data: outgoing, pos: 0, sizes, turn: 0 };
        let mut xf: Crossfader<Chunked, ()> = Crossfader::new(limiter());
        xf.begin(source, 1.0, ms_to_frames(fade_ms), (), limiter());
        assert!(matches!(xf.start_incoming(&(), true), IncomingStart::Fading(_)));
        let mut result = Vec::with_capacity(incoming.len());
        for block in incoming.chunks(1500 * CHANNELS) {
            play(&mut xf, block, &mut result);
        }
        result
    }

    // ---------- el fundido en el reproductor ----------
    //
    // Los pasos del reproductor con `Crossfader`, igual que en `player.rs`: cada paquete de la
    // canción que suena (`handle_packet`), los bloques de la saliente sola mientras la siguiente
    // carga (`write_tail`) y la suelta síncrona al cortar (`cancel_crossfade`). Lo escrito en la
    // salida se acumula en `out`.

    type Mixer = Crossfader<Box<dyn PacketSource>, &'static str>;

    impl PacketSource for Box<dyn PacketSource> {
        fn next_samples(&mut self) -> Option<Vec<f64>> {
            (**self).next_samples()
        }
    }

    fn mixer() -> Mixer {
        Crossfader::new(limiter())
    }

    /// Un paquete de la canción que suena: fuera de un fundido no se toca.
    fn play<S: PacketSource, T: PartialEq>(xf: &mut Crossfader<S, T>, packet: &[f64], out: &mut Vec<f64>) {
        let mut block = packet.to_vec();
        if xf.is_active() {
            if xf.process(&mut block) {
                xf.protect(&mut block, false);
            } else {
                xf.settle(&mut block);
            }
        }
        out.extend(block);
    }

    /// Un bloque de la saliente sola (la siguiente aún carga). `false` cuando ya no queda nada.
    fn write_tail(xf: &mut Mixer, out: &mut Vec<f64>) -> bool {
        match xf.tail() {
            Some(mut block) => {
                assert!(block.len() <= TAIL_FRAMES * CHANNELS);
                xf.protect(&mut block, false);
                out.extend(block);
                true
            }
            None => false,
        }
    }

    /// Corte durante el fundido: la suelta se escribe en el acto. Devuelve los marcos escritos.
    fn cut(xf: &mut Mixer, audible: bool, out: &mut Vec<f64>) -> usize {
        let mut frames = 0;
        for mut block in xf.cancel(audible) {
            xf.protect(&mut block, false);
            frames += block.len() / CHANNELS;
            out.extend(block);
        }
        assert!(!xf.is_active(), "tras cortar no queda nada del fundido");
        frames
    }

    fn constant(value: f64, size: usize) -> Box<dyn PacketSource> {
        Box::new(Constant { value, size })
    }

    /// Fundido ya en marcha hacia «b»: la saliente es una constante de 0,5 (bajo el limitador y la
    /// protección, así se ve exacta en la salida) y la entrante, silencio.
    fn fading_to_b(fade_frames: u64) -> Mixer {
        let mut xf = mixer();
        xf.begin(constant(0.5, 2048), 1.0, fade_frames, "b", limiter());
        match xf.start_incoming(&"b", true) {
            IncomingStart::Fading(r) => assert_eq!(r.total(), fade_frames.max(FADE_IN_MIN_FRAMES)),
            other => panic!("no empezó el fundido: {other:?}"),
        }
        xf
    }

    #[test]
    fn cut_mid_fade_releases_in_40_ms_and_no_outgoing_sample_follows() {
        let mut xf = fading_to_b(ms_to_frames(6000));
        let mut out = Vec::new();
        for _ in 0..20 {
            play(&mut xf, &[0.0; 1152 * CHANNELS], &mut out);
        }
        let before = out.len();
        let last = out[before - 1];
        assert!(last > 0.4, "la saliente debía sonar aún: {last}");

        // Siguiente a mano (Load con corte): suelta de 40 ms escrita ya, sin escalón.
        let released = cut(&mut xf, true, &mut out);
        assert_eq!(released, RELEASE_FRAMES as usize);
        let release = &out[before..];
        assert!((release[0] - last).abs() < 1e-3, "escalón al soltar: {last} → {}", release[0]);
        assert!(release.windows(2).all(|w| w[1] <= w[0]), "la suelta sube");
        assert!(release[release.len() - 1] < 1e-3);

        // La canción nueva (otra, sin fundido) sale intacta: ni una muestra de la anterior.
        assert_eq!(xf.start_incoming(&"c", true), IncomingStart::Plain);
        let after = out.len();
        for _ in 0..10 {
            play(&mut xf, &[0.25; 1000 * CHANNELS], &mut out);
        }
        assert!(out[after..].iter().all(|&s| s == 0.25));
    }

    #[test]
    fn cut_while_not_audible_drops_without_writing() {
        // En pausa (o con la salida parada) nadie oiría la suelta: se quita sin escribir nada.
        let mut xf = fading_to_b(ms_to_frames(6000));
        let mut out = Vec::new();
        play(&mut xf, &[0.0; 4096], &mut out);
        let before = out.len();
        assert_eq!(cut(&mut xf, false, &mut out), 0);
        assert_eq!(out.len(), before);
        assert_eq!(xf.start_incoming(&"b", true), IncomingStart::Plain);
        let after = out.len();
        play(&mut xf, &[0.75; 4096], &mut out);
        assert!(out[after..].iter().all(|&s| s == 0.75));
    }

    #[test]
    fn pause_keeps_both_decks() {
        // El mezclador no cuenta tiempo: lo que suena depende solo de los bloques que recibe. La
        // pausa del reproductor (que no escribe ni mezcla nada) conserva las dos canciones y, al
        // reanudar, el fundido sigue exactamente donde iba, como si no hubiera habido pausa.
        let packet: Vec<f64> = (0..1000 * CHANNELS).map(|i| (i as f64 * 0.01).sin() * 0.3).collect();
        let mut seguido = fading_to_b(ms_to_frames(1000));
        let mut con_pausa = fading_to_b(ms_to_frames(1000));
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for i in 0..60 {
            play(&mut seguido, &packet, &mut a);
            // A mitad: pausa (nada se escribe ni se mezcla) y luego reanudar.
            if i == 20 {
                assert!(con_pausa.has_outgoing() && con_pausa.fade_in().is_some());
            }
            play(&mut con_pausa, &packet, &mut b);
        }
        assert!(a == b);
        assert!(!seguido.is_active());
    }

    #[test]
    fn failed_load_lets_the_outgoing_play_out_and_a_later_track_has_no_stale_tail() {
        // La siguiente no llega (sigue cargando o falló): la saliente se escribe sola en bloques
        // pequeños hasta acabar su rampa, y desaparece.
        let fade = ms_to_frames(1000);
        let mut xf = mixer();
        xf.begin(constant(0.5, 2048), 1.0, fade, "b", limiter());
        let mut out = Vec::new();
        let mut blocks = 0;
        while write_tail(&mut xf, &mut out) {
            blocks += 1;
            assert!(blocks < 100, "la saliente no termina");
        }
        assert_eq!(out.len() / CHANNELS, fade as usize);
        assert!(out.windows(2).all(|w| w[1] <= w[0]));
        assert!(!xf.is_active());

        // Más tarde, otra canción: empieza limpia, sin la cola de la anterior.
        assert_eq!(xf.start_incoming(&"c", true), IncomingStart::Plain);
        let after = out.len();
        play(&mut xf, &[0.1; 2048], &mut out);
        assert!(out[after..].iter().all(|&s| s == 0.1));
    }

    #[test]
    fn incoming_that_starts_paused_or_is_another_track_drops_the_outgoing() {
        // La esperada, pero empieza en pausa: nadie escribiría la saliente.
        let mut xf = mixer();
        xf.begin(constant(0.5, 2048), 1.0, 44_100, "b", limiter());
        assert_eq!(xf.start_incoming(&"b", false), IncomingStart::Dropped);
        assert!(!xf.is_active());

        // Otra canción que la esperada.
        xf.begin(constant(0.5, 2048), 1.0, 44_100, "b", limiter());
        assert_eq!(xf.start_incoming(&"x", true), IncomingStart::Dropped);
        assert!(!xf.is_active());

        // Ninguna de las dos deja nada para la siguiente.
        let mut out = Vec::new();
        play(&mut xf, &[0.2; 4096], &mut out);
        assert!(out.iter().all(|&s| s == 0.2));
    }

    #[test]
    fn auto_skip_keeps_the_outgoing_ramp_and_the_next_joins_with_what_is_left() {
        let fade = 20_000u64;
        let mut xf = fading_to_b(fade);
        let mut out = Vec::new();
        // 5000 marcos de fundido con «b».
        for _ in 0..5 {
            play(&mut xf, &[0.0; 1000 * CHANNELS], &mut out);
        }
        // «b» estaba oculta: salto automático a «c». La saliente sigue; la entrada se descarta.
        assert!(xf.auto_skip("c"));
        assert!(xf.has_outgoing() && xf.fade_in().is_none());
        // «c» carga: 3 bloques de la saliente sola.
        for _ in 0..3 {
            assert!(write_tail(&mut xf, &mut out));
        }
        let done = (out.len() / CHANNELS) as u64;
        assert_eq!(done, 5000 + 3 * TAIL_FRAMES as u64);
        // «c» entra con lo que le queda a la rampa de salida, ni más ni menos.
        match xf.start_incoming(&"c", true) {
            IncomingStart::Fading(r) => assert_eq!(r.total(), fade - done),
            other => panic!("«c» no entró con fundido: {other:?}"),
        }
        while xf.is_active() {
            play(&mut xf, &[0.0; 777 * CHANNELS], &mut out);
        }
        // La saliente no notó el salto: su rampa siguió marco a marco sin volver a empezar.
        let mut reference = Ramp::new(fade);
        for (f, frame) in out.chunks(CHANNELS).enumerate() {
            let want = 0.5 * reference.gain_out();
            reference.advance();
            assert!((frame[0] - want).abs() < 1e-15 && frame[1] == frame[0], "marco {f}");
        }
        assert!(out.len() / CHANNELS >= fade as usize);

        // Sin fundido que conservar (ya acabado, o soltándose) no hay salto automático.
        assert!(!xf.auto_skip("d"));
        let mut xf = fading_to_b(fade);
        xf.cancel(false);
        assert!(!xf.auto_skip("d"));
    }

    #[test]
    fn outgoing_eof_before_its_ramp_end_leaves_only_the_fade_in() {
        // El fichero de la saliente se acaba antes que su rampa (la duración de los metadatos era
        // algo mayor): la entrante sigue su rampa sola, sin protección (ya no hay suma) y, al
        // acabarla, vuelve a salir intacta.
        let mut xf = mixer();
        xf.begin(Box::new(Packets::new(vec![vec![0.5; 200]])) as Box<dyn PacketSource>, 1.0, 1000, "b", limiter());
        assert!(matches!(xf.start_incoming(&"b", true), IncomingStart::Fading(_)));
        let mut block = vec![1.0; 300 * CHANNELS];
        assert!(xf.process(&mut block), "la saliente sonaba en el primer bloque");
        assert!(!xf.has_outgoing());
        let mut block = vec![1.0; 300 * CHANNELS];
        assert!(!xf.process(&mut block), "sin saliente no hay suma que proteger");
        assert!(block[0] < 1.0 && xf.is_active());
        let mut out = Vec::new();
        let mut packets = 0;
        while xf.is_active() {
            play(&mut xf, &[1.0; 300 * CHANNELS], &mut out);
            packets += 1;
        }
        // La entrada dura lo mínimo antichasquido (2205 marcos), no lo que le quedaba a la saliente.
        assert_eq!(600 + packets * 300, FADE_IN_MIN_FRAMES.div_ceil(300) * 300);
        assert!(out.windows(2).all(|w| w[1] >= w[0]) && out[out.len() - 1] == 1.0);
        let after = out.len();
        play(&mut xf, &[1.0; 300 * CHANNELS], &mut out);
        assert!(out[after..].iter().all(|&s| s == 1.0));
    }

    #[test]
    fn a_mixer_that_is_not_active_never_touches_the_audio() {
        // Con el fundido apagado (o terminado) el audio sale bit a bit como entra, también valores
        // por encima de la escala, que fuera de un fundido no son cosa del mezclador.
        let packet: Vec<f64> = (0..4096).map(|i| ((i as f64) * 0.37).sin() * 1.2).collect();
        let mut xf = mixer();
        let mut out = Vec::new();
        play(&mut xf, &packet, &mut out);
        assert!(out == packet);
        assert_eq!(xf.start_incoming(&"a", true), IncomingStart::Plain);
        assert!(xf.tail().is_none());
        assert!(xf.cancel(true).is_empty());
        assert!(!xf.clear());

        // Tras un fundido completo, igual.
        let mut xf = fading_to_b(2000);
        while xf.is_active() {
            play(&mut xf, &[0.0; 512], &mut Vec::new());
        }
        let mut out = Vec::new();
        play(&mut xf, &packet, &mut out);
        assert!(out == packet);
    }

    #[test]
    fn release_of_a_long_fade_is_two_blocks() {
        let mut xf = fading_to_b(ms_to_frames(CROSSFADE_MAX_MS));
        let blocks = xf.cancel(true);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks.iter().map(|b| b.len() / CHANNELS).sum::<usize>(), RELEASE_FRAMES as usize);
        assert!(!xf.is_active());
    }

    #[test]
    fn clear_drops_everything_silently() {
        let mut xf = fading_to_b(44_100);
        assert!(xf.clear());
        assert!(!xf.is_active());
        assert!(xf.tail().is_none());
        assert!(!xf.auto_skip("c"));
    }

    #[test]
    fn equal_power_keeps_summed_pink_noise_power_within_half_a_db() {
        // Dos ruidos rosas de potencia constante (−20 dBFS en cada bloque de medida) y no
        // correlados en cada bloque (Gram-Schmidt): la potencia de la mezcla tiene que seguir
        // siendo la misma a lo largo de todo el fundido. Uno lineal bajaría 3 dB a mitad.
        const BLOCK: usize = 8192 * CHANNELS;
        let fade_ms = 6000;
        let frames = 34 * 8192;
        let rms = 0.1;
        let mut a = pink(1, frames);
        let mut b = pink(2, frames);
        for (ba, bb) in a.chunks_mut(BLOCK).zip(b.chunks_mut(BLOCK)) {
            scale_to_rms(ba, rms);
            let k = dot(ba, bb) / dot(ba, ba);
            bb.iter_mut().zip(ba.iter()).for_each(|(y, x)| *y -= k * x);
            scale_to_rms(bb, rms);
        }

        let mixed = crossfade(a, &b, fade_ms);
        assert_eq!(mixed.len(), b.len());
        let peak = mixed.iter().fold(0.0f64, |m, x| m.max(x.abs()));
        assert!(peak <= 1.0, "pico {peak}");
        let mut worst = 0.0f64;
        for (i, block) in mixed.chunks(BLOCK).enumerate() {
            let dev = db(power(block) / (rms * rms));
            assert!(dev.abs() <= 0.5, "bloque {i}: {dev:+.3} dB");
            worst = worst.max(dev.abs());
        }
        // Con fuentes así de limpias la desviación real es mucho menor que la tolerancia.
        assert!(worst < 0.1, "peor desviación {worst:.3} dB");
    }

    #[test]
    fn full_scale_noise_mix_never_passes_full_scale() {
        // Dos ruidos blancos a fondo de escala: a mitad del fundido la suma llega a ~1,41.
        let frames = ms_to_frames(1500) as usize;
        let mut ra = Rng(7);
        let mut rb = Rng(8);
        let a: Vec<f64> = (0..frames * CHANNELS).map(|_| ra.next()).collect();
        let b: Vec<f64> = (0..frames * CHANNELS).map(|_| rb.next()).collect();
        let mixed = crossfade(a, &b, 1000);
        let peak = mixed.iter().fold(0.0f64, |m, x| m.max(x.abs()));
        assert!(peak <= 1.0, "pico {peak}");
    }

    // ---------- limitador y protección ----------

    fn sine(amp: f64, hz: f64, frames: usize) -> Vec<f64> {
        let w = 2.0 * PI * hz / SAMPLE_RATE as f64;
        (0..frames)
            .flat_map(|n| {
                let s = amp * (w * n as f64).sin();
                [s, s]
            })
            .collect()
    }

    #[test]
    fn mix_below_threshold_is_bit_exact() {
        // Hasta 0,88 (−1,1 dBFS, bajo el inicio de la rodilla en −1 dBFS) ni el limitador ni la
        // protección cambian un solo bit; tampoco el silencio.
        let mut data = sine(0.88, 997.0, 44_100);
        data.extend(vec![0.0; 2000]);
        let original = data.clone();
        let mut lim = limiter();
        mix_limit(&mut data, &mut lim);
        data.iter_mut().for_each(|s| *s = safety_clamp(*s));
        assert!(data == original, "la mezcla bajo el umbral cambió");
        assert_eq!(lim.reduction_db(), 0.0);
    }

    #[test]
    fn limiter_holds_a_sustained_over_near_the_threshold() {
        // Seno a 1,3 (+2,3 dBFS): pasados 50 ms los picos quedan en torno a −0,5 dBFS.
        let mut data = sine(1.3, 1000.0, 22_050);
        let mut lim = limiter();
        mix_limit(&mut data, &mut lim);
        let settled = &data[ms_to_frames(50) as usize * CHANNELS..];
        let peak = settled.iter().fold(0.0f64, |m, x| m.max(x.abs()));
        let peak_db = 20.0 * peak.log10();
        assert!((-1.0..=-0.3).contains(&peak_db), "pico estable {peak_db:.2} dBFS");
        // Los dos canales llevan la misma reducción.
        assert!(data.chunks(2).all(|f| f[0] == f[1]));
    }

    #[test]
    fn limiter_releases_back_to_unity_and_reset_clears_it() {
        let mut lim = limiter();
        let mut loud = sine(1.3, 1000.0, 4410);
        mix_limit(&mut loud, &mut lim);
        assert!(lim.reduction_db() > 2.0);

        // Dos segundos después de que pase, la ganancia ha vuelto a menos de 0,01 dB de 1 (la
        // liberación por canal avanza una vez por marco: unos 200 ms de constante de tiempo).
        let mut quiet = sine(0.2, 1000.0, 88_200);
        let original = quiet.clone();
        mix_limit(&mut quiet, &mut lim);
        let tail = 84_000 * CHANNELS;
        for (q, o) in quiet[tail..].iter().zip(&original[tail..]) {
            if o.abs() > 1e-3 {
                assert!((20.0 * (q / o).log10()).abs() < 0.01);
            }
        }

        // Tras `reset` (inicio de otro fundido) no queda nada de lo anterior.
        let mut lim = limiter();
        let mut loud = sine(1.3, 1000.0, 4410);
        mix_limit(&mut loud, &mut lim);
        lim.reset();
        let mut quiet = sine(0.2, 1000.0, 4410);
        let original = quiet.clone();
        mix_limit(&mut quiet, &mut lim);
        assert!(quiet == original);
    }

    #[test]
    fn mix_limiter_lets_go_smoothly_when_the_outgoing_ends() {
        // Dos másteres a −0,26 dBFS (sin normalización): al acabar el fundido el limitador de la
        // mezcla aún reduce los picos de la entrante. En el primer bloque sin suma esa reducción
        // tiene que soltarse poco a poco (`settle`), no desaparecer de un marco al siguiente.
        let fade = ms_to_frames(500);
        let total = ms_to_frames(4000) as usize;
        let incoming = sine(0.97, 997.0, total);
        let source = Chunked { data: sine(0.97, 440.0, total), pos: 0, sizes: vec![2048], turn: 0 };
        let mut xf = mixer();
        xf.begin(Box::new(source) as Box<dyn PacketSource>, 1.0, fade, "b", limiter());
        assert!(matches!(xf.start_incoming(&"b", true), IncomingStart::Fading(_)));
        let mut out = Vec::new();
        let mut reduction_at_end = 0.0;
        for block in incoming.chunks(1000 * CHANNELS) {
            let was_fading = xf.is_fading();
            play(&mut xf, block, &mut out);
            if was_fading && !xf.is_fading() {
                reduction_at_end = xf.limiter.reduction_db();
            }
        }
        assert!(reduction_at_end > 0.1, "la prueba no llega a limitar: {reduction_at_end} dB");
        assert!(!xf.is_active(), "el limitador no se soltó");

        // Ganancia de la entrante sola, marco a marco, desde que acaba el fundido: sin saltos.
        let mut last: Option<f64> = None;
        for f in fade as usize..total {
            let x = incoming[f * CHANNELS];
            if x.abs() < 0.3 {
                continue;
            }
            let g = out[f * CHANNELS] / x;
            assert!(g <= 1.0 + 1e-12, "marco {f}: ganancia {g}");
            if let Some(l) = last {
                assert!((g - l).abs() < 1e-3, "salto de ganancia en el marco {f}: {l} → {g}");
            }
            last = Some(g);
        }
        // Y al final vuelve a salir bit a bit como entra.
        let tail = (total - 1000) * CHANNELS;
        assert!(out[tail..] == incoming[tail..]);
    }

    #[test]
    fn hard_knee_limiter_has_no_nan() {
        let mut lim =
            LimiterState::new(MIX_LIMIT_THRESHOLD_DBFS, 0.0, coefficient(5.0), coefficient(100.0));
        let t = 10f64.powf(MIX_LIMIT_THRESHOLD_DBFS / 20.0);
        let mut data = vec![t, t, 1.5, -1.5, 0.0, 0.0];
        mix_limit(&mut data, &mut lim);
        assert!(data.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn non_finite_samples_do_not_jam_the_limiter() {
        let mut lim = limiter();
        let mut bad = vec![f64::INFINITY, f64::NAN, f64::NEG_INFINITY, 0.5];
        mix_limit(&mut bad, &mut lim);
        assert_eq!(lim.reduction_db(), 0.0);
        // Lo que sigue suena igual que si no hubieran llegado.
        let mut quiet = sine(0.5, 1000.0, 441);
        let original = quiet.clone();
        mix_limit(&mut quiet, &mut lim);
        assert!(quiet == original);
        // Y la protección final las deja dentro de la escala.
        let clamped: Vec<f64> = bad.iter().map(|&s| safety_clamp(s)).collect();
        assert_eq!(clamped, [1.0, 0.0, -1.0, 0.5]);
    }

    #[test]
    fn safety_clamp_is_transparent_below_the_knee_and_bounded() {
        for &x in &[0.0, 0.5, -0.5, 0.97, -0.98, 0.98] {
            assert_eq!(safety_clamp(x), x);
        }
        let mut last = SAFETY_CLAMP_KNEE;
        for i in 1..=2000 {
            let x = SAFETY_CLAMP_KNEE + i as f64 * 0.005;
            let y = safety_clamp(x);
            assert!(y > SAFETY_CLAMP_KNEE && y <= 1.0 && y >= last, "{x} → {y}");
            assert_eq!(safety_clamp(-x), -y);
            last = y;
        }
        // Sin escalón: justo encima del codo sigue casi con pendiente 1.
        let x = SAFETY_CLAMP_KNEE + 1e-6;
        assert!((safety_clamp(x) - x).abs() < 1e-9);
        assert_eq!(safety_clamp(f64::INFINITY), 1.0);
        assert_eq!(safety_clamp(f64::NEG_INFINITY), -1.0);
        assert_eq!(safety_clamp(f64::NAN), 0.0);
    }

    // ---------- cuándo y cuánto ----------

    #[test]
    fn plan_fade_ms_cases() {
        // Ya dentro de la ventana: lo que queda.
        assert_eq!(plan_fade_ms(6000, 5000, 240_000, 200_000), Some(5000));
        // Aún no (más que fundido + antelación).
        assert_eq!(plan_fade_ms(6000, 7000, 240_000, 200_000), None);
        // Dentro de la antelación: el fundido entero.
        assert_eq!(plan_fade_ms(6000, 6700, 240_000, 200_000), Some(6000));
        assert_eq!(plan_fade_ms(6000, 6750, 240_000, 200_000), Some(6000));
        assert_eq!(plan_fade_ms(6000, 6751, 240_000, 200_000), None);
        // Tope en media duración de la siguiente y de la actual.
        assert_eq!(plan_fade_ms(6000, 4500, 240_000, 8000), Some(4000));
        assert_eq!(plan_fade_ms(6000, 4800, 240_000, 8000), None);
        assert_eq!(plan_fade_ms(6000, 2500, 5000, 200_000), Some(2500));
        // Tope en 12 s.
        assert_eq!(plan_fade_ms(20_000, 12_500, 240_000, 200_000), Some(12_000));
        // Menos de 1 s: nada (sin hueco, como siempre).
        assert_eq!(plan_fade_ms(900, 500, 240_000, 200_000), None);
        assert_eq!(plan_fade_ms(6000, 1000, 240_000, 1800), None);
        assert_eq!(plan_fade_ms(6000, 800, 240_000, 200_000), None);
        assert_eq!(plan_fade_ms(6000, 1000, 240_000, 200_000), Some(1000));
        // Apagado o duraciones desconocidas.
        assert_eq!(plan_fade_ms(0, 500, 240_000, 200_000), None);
        assert_eq!(plan_fade_ms(6000, 500, 0, 0), None);
        assert_eq!(plan_fade_ms(u32::MAX, u32::MAX, u32::MAX, u32::MAX), None);
    }

    #[test]
    fn frame_conversions() {
        assert_eq!(ms_to_frames(40), RELEASE_FRAMES);
        assert_eq!(ms_to_frames(50), FADE_IN_MIN_FRAMES);
        assert_eq!(ms_to_frames(CROSSFADE_MAX_MS), 529_200);
        assert_eq!(frames_to_ms(ms_to_frames(6000)), 6000);
        assert_eq!(ms_to_frames(u32::MAX), u32::MAX as u64 * 441 / 10);
    }

    // ---------- regla del álbum ----------

    fn track(album: &str, artists: &[&str], disc: u32, number: u32) -> UniqueFields {
        UniqueFields::Track {
            artists: ArtistsWithRole::default(),
            album: album.to_string(),
            album_artists: artists.iter().map(|s| s.to_string()).collect(),
            popularity: 50,
            number,
            disc_number: disc,
        }
    }

    fn local(
        album: Option<&str>,
        artist: Option<&str>,
        disc: Option<u32>,
        number: Option<u32>,
    ) -> UniqueFields {
        UniqueFields::Local {
            artists: None,
            album: album.map(String::from),
            album_artists: artist.map(String::from),
            number,
            disc_number: disc,
            path: PathBuf::from("x.flac"),
        }
    }

    #[test]
    fn album_continuation_cases() {
        let da = ["Daft Punk"];
        let ram = |disc, n| track("Random Access Memories", &da, disc, n);
        // n → n+1 en el mismo disco.
        assert!(is_album_continuation(&ram(1, 7), &ram(1, 8)));
        // Fin del disco 1 → pista 1 del disco 2.
        assert!(is_album_continuation(&ram(1, 13), &ram(2, 1)));
        // n → n+2, hacia atrás, la misma, o pista 2 del disco siguiente.
        assert!(!is_album_continuation(&ram(1, 7), &ram(1, 9)));
        assert!(!is_album_continuation(&ram(1, 8), &ram(1, 7)));
        assert!(!is_album_continuation(&ram(1, 7), &ram(1, 7)));
        assert!(!is_album_continuation(&ram(1, 13), &ram(2, 2)));
        assert!(!is_album_continuation(&ram(1, 13), &ram(3, 1)));
        // Otro álbum, u otros artistas con un álbum del mismo nombre.
        assert!(!is_album_continuation(&ram(1, 7), &track("Discovery", &da, 1, 8)));
        assert!(!is_album_continuation(
            &ram(1, 7),
            &track("Random Access Memories", &["Otro"], 1, 8)
        ));
        // Sin datos con los que decidir.
        assert!(!is_album_continuation(&track("", &da, 1, 1), &track("", &da, 1, 2)));
        assert!(!is_album_continuation(&ram(1, 0), &ram(1, 1)));
        assert!(!is_album_continuation(&ram(1, u32::MAX), &ram(1, 0)));
        assert!(!is_album_continuation(&ram(u32::MAX, 3), &ram(0, 1)));
    }

    #[test]
    fn album_continuation_local_files_and_episodes() {
        let l = |n| local(Some("Álbum"), Some("Artista"), Some(1), Some(n));
        assert!(is_album_continuation(&l(3), &l(4)));
        assert!(!is_album_continuation(&l(3), &l(5)));
        // Sin todas las etiquetas no se decide nada.
        let sin_disco = |n| local(Some("Álbum"), Some("Artista"), None, Some(n));
        assert!(!is_album_continuation(&sin_disco(3), &sin_disco(4)));
        // Una canción de Spotify y un archivo local nunca se consideran del mismo álbum.
        let t = track("Álbum", &["Artista"], 1, 3);
        assert!(!is_album_continuation(&t, &l(4)));
        // Pódcast: nunca.
        let ep = UniqueFields::Episode {
            description: String::new(),
            publish_time: librespot_core::date::Date::now_utc(),
            show_name: "Programa".into(),
        };
        assert!(!is_album_continuation(&ep, &ep.clone()));
        assert!(!is_album_continuation(&t, &ep));
    }

    #[test]
    fn transition_defaults_to_cut() {
        assert_eq!(Transition::default(), Transition::Cut);
    }

    // ---------- decisión de Spirc ----------

    /// Una propuesta que se acepta: sonando, cola al día, a 6,5 s del final con 6 s de fundido,
    /// canciones de una playlist (sin regla de álbum).
    fn offer() -> FadeOffer {
        FadeOffer {
            playing: true,
            repeat_track: false,
            jam_participant: false,
            already_accepted: false,
            queue_matches: true,
            remaining_ms: Some(6500),
            fade_ms: 6000,
            album_in_order: false,
            album_continuation: false,
            crossfade_albums: false,
        }
    }

    #[test]
    fn spirc_accepts_a_natural_transition() {
        assert_eq!(decide_offer(&offer()), Ok(()));
        // Sin duración conocida no se puede comprobar la posición: se acepta igual.
        assert_eq!(
            decide_offer(&FadeOffer {
                remaining_ms: None,
                ..offer()
            }),
            Ok(())
        );
        // Lo que quede por debajo de lo previsto (la respuesta tardó) también vale.
        assert_eq!(
            decide_offer(&FadeOffer {
                remaining_ms: Some(0),
                ..offer()
            }),
            Ok(())
        );
    }

    #[test]
    fn spirc_declines_outside_natural_transitions() {
        let declined = |o: FadeOffer| decide_offer(&o).unwrap_err();
        assert_eq!(
            declined(FadeOffer {
                playing: false,
                ..offer()
            }),
            FadeDecline::NotPlaying
        );
        assert_eq!(
            declined(FadeOffer {
                repeat_track: true,
                ..offer()
            }),
            FadeDecline::RepeatTrack
        );
        assert_eq!(
            declined(FadeOffer {
                jam_participant: true,
                ..offer()
            }),
            FadeDecline::Jam
        );
        // La misma propuesta otra vez (tras aceptarla): una sola carga, nunca dos saltos.
        assert_eq!(
            declined(FadeOffer {
                already_accepted: true,
                ..offer()
            }),
            FadeDecline::AlreadyAccepted
        );
        assert_eq!(
            declined(FadeOffer {
                queue_matches: false,
                ..offer()
            }),
            FadeDecline::QueueChanged
        );
        // Repetir canción gana a todo lo demás, también con «fundir álbumes».
        assert_eq!(
            declined(FadeOffer {
                repeat_track: true,
                crossfade_albums: true,
                ..offer()
            }),
            FadeDecline::RepeatTrack
        );
    }

    #[test]
    fn spirc_declines_a_stale_offer_after_seeking_back() {
        // Ventana: fundido + antelación + margen de la cuenta de Spirc.
        let window = 6000 + CROSSFADE_LEAD_MS + OFFER_POSITION_SLACK_MS;
        let at = |left| {
            decide_offer(&FadeOffer {
                remaining_ms: Some(left),
                ..offer()
            })
        };
        assert_eq!(at(window), Ok(()));
        assert_eq!(at(window + 1), Err(FadeDecline::NotNearEnd));
        // Se buscó a mitad de canción mientras la propuesta viajaba.
        assert_eq!(at(120_000), Err(FadeDecline::NotNearEnd));
        // Sin desbordes con valores extremos.
        assert_eq!(
            decide_offer(&FadeOffer {
                fade_ms: u32::MAX,
                remaining_ms: Some(u32::MAX),
                ..offer()
            }),
            Ok(())
        );
    }

    #[test]
    fn spirc_album_rule() {
        let album = FadeOffer {
            album_in_order: true,
            ..offer()
        };
        let same_album_playlist = FadeOffer {
            album_continuation: true,
            ..offer()
        };
        // Un álbum en orden, o una playlist ordenada como el álbum: sin fundido…
        assert_eq!(decide_offer(&album), Err(FadeDecline::SameAlbum));
        assert_eq!(decide_offer(&same_album_playlist), Err(FadeDecline::SameAlbum));
        // …salvo con «fundir también canciones seguidas de un mismo álbum».
        assert_eq!(
            decide_offer(&FadeOffer {
                crossfade_albums: true,
                ..album
            }),
            Ok(())
        );
        assert_eq!(
            decide_offer(&FadeOffer {
                crossfade_albums: true,
                ..same_album_playlist
            }),
            Ok(())
        );
    }

    #[test]
    fn album_in_order_needs_album_context_in_order() {
        let album = "spotify:album:4m2880jivSbbyEGAKfITCa";
        assert!(is_album_in_order(album, false, true, true));
        // En aleatorio el orden del artista ya no está: se funde.
        assert!(!is_album_in_order(album, true, true, true));
        // Una canción de la cola o de autoplay (al acabarse el álbum) no es del álbum.
        assert!(!is_album_in_order(album, false, true, false));
        assert!(!is_album_in_order(album, false, false, true));
        // Playlists, artistas, radios o sin contexto.
        assert!(!is_album_in_order("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M", false, true, true));
        assert!(!is_album_in_order("spotify:artist:4tZwfgrHOc3mvqYlEYSvVi", false, true, true));
        assert!(!is_album_in_order("", false, true, true));
    }
}
