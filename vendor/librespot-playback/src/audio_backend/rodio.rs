use std::process::exit;
use std::thread;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Volumen lineal (0.0..=1.0) aplicado directamente en el sink de rodio, de modo que el
/// cambio se oye al instante en vez de tras vaciar la cola de paquetes ya decodificados.
static OUTPUT_VOLUME: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0f32

pub fn set_output_volume(volume: f32) {
    OUTPUT_VOLUME.store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
}

fn output_volume() -> f32 {
    f32::from_bits(OUTPUT_VOLUME.load(Ordering::Relaxed))
}

/// Cómo quedó la última salida abierta, para enseñarlo en la interfaz sin preguntar al hilo del
/// reproductor: frecuencia del dispositivo (0 = aún no se ha abierto ninguna), canales y si el
/// paso desde 44,1 kHz lo hace nuestro remuestreador sinc.
static OUTPUT_RATE: AtomicU32 = AtomicU32::new(0);
static OUTPUT_CH: AtomicU16 = AtomicU16::new(0);
static RESAMPLED: AtomicBool = AtomicBool::new(false);

/// Hay salida de audio: falso cuando el último intento de sonar (`start`, `write`) no encontró
/// ningún dispositivo. Empieza en verdadero (aún no se sabe) y lo vuelve a poner `probe_output`
/// al aparecer uno.
static OUTPUT_OK: AtomicBool = AtomicBool::new(true);

/// ¿Encontró dispositivo el último intento de sonar? Con `false`, la canción se dejó en pausa por
/// falta de salida (ver `RodioSink::start`).
pub fn output_ok() -> bool {
    OUTPUT_OK.load(Ordering::Relaxed)
}

/// ¿Hay ahora algún dispositivo de salida? Si lo hay se da la salida por buena (`output_ok`).
/// Mira el dispositivo predeterminado del sistema (unos milisegundos): para llamarlo de vez en
/// cuando mientras falta la salida, no en cada fotograma.
pub fn probe_output() -> bool {
    let found = !crate::core::fault::no_output()
        && cpal::default_host().default_output_device().is_some();
    if found {
        OUTPUT_OK.store(true, Ordering::Relaxed);
    }
    found
}

/// (frecuencia del dispositivo en Hz, canales, remuestreo sinc propio) de la última salida
/// abierta. Frecuencia 0: todavía no ha sonado nada. Frecuencia distinta de 44 100 sin sinc: la
/// conversión la hace rodio (NANOFY_RESAMPLER=rodio o una relación de frecuencias no soportada).
pub fn output_info() -> (u32, u16, bool) {
    (
        OUTPUT_RATE.load(Ordering::Relaxed),
        OUTPUT_CH.load(Ordering::Relaxed),
        RESAMPLED.load(Ordering::Relaxed),
    )
}

use cpal::traits::{DeviceTrait, HostTrait};
use thiserror::Error;

use super::out_queue::{self, ConsumeCounter, FadeIn, QueueClock};
use super::resample::{self, Resampler};
use super::{Sink, SinkError, SinkResult};
use crate::config::AudioFormat;
use crate::convert::Converter;
use crate::decoder::AudioPacket;
use crate::{NUM_CHANNELS, SAMPLE_RATE};

#[cfg(all(
    feature = "rodiojack-backend",
    not(any(target_os = "linux", target_os = "dragonfly", target_os = "freebsd"))
))]
compile_error!("Rodio JACK backend is currently only supported on linux.");

#[cfg(feature = "rodio-backend")]
pub fn mk_rodio(device: Option<String>, format: AudioFormat) -> Box<dyn Sink> {
    Box::new(open(cpal::default_host(), device, format))
}

#[cfg(feature = "rodiojack-backend")]
pub fn mk_rodiojack(device: Option<String>, format: AudioFormat) -> Box<dyn Sink> {
    Box::new(open(
        cpal::host_from_id(cpal::HostId::Jack).unwrap(),
        device,
        format,
    ))
}

#[derive(Debug, Error)]
pub enum RodioError {
    #[error("<RodioSink> No Device Available")]
    NoDeviceAvailable,
    #[error("<RodioSink> device \"{0}\" is Not Available")]
    DeviceNotAvailable(String),
    #[error("<RodioSink> Play Error: {0}")]
    PlayError(#[from] rodio::PlayError),
    #[error("<RodioSink> Stream Error: {0}")]
    StreamError(#[from] rodio::StreamError),
    #[error("<RodioSink> Cannot Get Audio Devices: {0}")]
    DevicesError(#[from] cpal::DevicesError),
    #[error("<RodioSink> {0}")]
    Samples(String),
}

impl From<RodioError> for SinkError {
    fn from(e: RodioError) -> SinkError {
        use RodioError::*;
        let es = e.to_string();
        match e {
            StreamError(_) | PlayError(_) | Samples(_) => SinkError::OnWrite(es),
            NoDeviceAvailable | DeviceNotAvailable(_) => SinkError::ConnectionRefused(es),
            DevicesError(_) => SinkError::InvalidParams(es),
        }
    }
}

impl From<cpal::DefaultStreamConfigError> for RodioError {
    fn from(_: cpal::DefaultStreamConfigError) -> RodioError {
        RodioError::NoDeviceAvailable
    }
}

impl From<cpal::SupportedStreamConfigsError> for RodioError {
    fn from(_: cpal::SupportedStreamConfigsError) -> RodioError {
        RodioError::NoDeviceAvailable
    }
}

/// Si la cola no baja en este tiempo damos la salida por perdida. Generoso a propósito: cada
/// reapertura tira lo que hubiera en cola, así que un falso positivo se oye como un corte.
const STALL_TIMEOUT: Duration = Duration::from_secs(5);
/// Tiempo mínimo entre reaperturas, para no encadenarlas descartando audio sin parar.
const REOPEN_COOLDOWN: Duration = Duration::from_secs(5);
/// Cuánto insistir en abrir la salida antes de devolver el control al reproductor. Eran 10 s con
/// el hilo del reproductor bloqueado (ni pausa ni siguiente) y, sin dispositivo, luego se tiraba
/// el audio en silencio con la barra en «sonando». Ahora, pasado esto sin salida, la canción queda
/// en pausa con su aviso y Nanofy la reanuda sola al volver un dispositivo, así que esperar más no
/// ayuda: un cambio de dispositivo normal (Bluetooth, USB) deja otro predeterminado al momento.
const REOPEN_TIMEOUT: Duration = Duration::from_millis(1500);

/// Salida de audio abierta: el sink de rodio y el flujo de cpal que lo alimenta.
struct Output {
    sink: rodio::Sink,
    stream: rodio::OutputStream,
    /// cpal avisa por aquí de que el flujo murió (dispositivo desconectado). Uno por flujo: el
    /// del anterior sigue avisando mientras cpal lo cierra y daría por muerto al recién abierto.
    dead: Arc<AtomicBool>,
    /// Nombre del dispositivo en el que se abrió (para seguir al predeterminado de Windows).
    device_name: String,
    /// Frecuencia y canales con que el mezclador de rodio alimenta al dispositivo.
    rate: u32,
    channels: u16,
    /// De 44,1 kHz a `rate`; `None` si el dispositivo ya va a 44,1 kHz o si la conversión se deja
    /// a rodio. Vive y muere con el flujo: al reabrir (dispositivo perdido, pausa larga, cambio
    /// del predeterminado) empieza de cero sin que haga falta reiniciarlo a mano en cada camino.
    resampler: Option<Resampler>,
    /// Lo entregado a rodio y lo que el dispositivo ya tomó: lo que queda en cola sin sonar
    /// (`queued_ms`). Uno nuevo con cada flujo y al vaciar la cola.
    clock: QueueClock,
}

impl Output {
    fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Relaxed)
    }

    /// Canales de los búferes que se entregan a rodio: uno si el dispositivo es mono (la mezcla
    /// se hace aquí); si no, estéreo, y rodio lo pone en el par delantero de los que haya.
    fn src_channels(&self) -> u16 {
        src_channels(self.channels)
    }

    /// Cuenta nueva de la cola, a la frecuencia y con los canales de los búferes que se entregan.
    fn new_clock(&self) -> QueueClock {
        let rate = if self.resampler.is_some() { self.rate } else { SAMPLE_RATE };
        QueueClock::new(rate, self.src_channels())
    }

    /// Lo encolado se va sin sonar: un sink de rodio nuevo sobre el mismo flujo, con el mismo
    /// volumen y en el mismo estado (sonando o en pausa). `rodio::Sink::clear` no sirve: espera a
    /// que el hilo de audio pase por cada búfer en cola, y con el dispositivo caído nadie pasa y
    /// el reproductor se quedaría colgado. El sink viejo, al soltarse, se calla en su siguiente
    /// comprobación (cada 5 ms). El remuestreador olvida lo que retenía: lo siguiente no continúa.
    fn discard_queue(&mut self) {
        if !self.sink.empty() {
            let sink = rodio::Sink::connect_new(self.stream.mixer());
            sink.set_volume(self.sink.volume());
            if self.sink.is_paused() {
                sink.pause();
            }
            self.sink = sink;
        }
        if let Some(r) = &mut self.resampler {
            r.reset();
        }
        self.clock = self.new_clock();
    }

    /// Encola un paquete del reproductor (f64 estéreo a 44,1 kHz) ya en el formato del
    /// dispositivo. Con la frecuencia y los canales del mezclador, el conversor de rodio queda en
    /// paso directo: el suyo es lineal y se reinicia en cada paquete (agudos apagados y
    /// granulosos).
    fn append(&mut self, samples: &[f64]) {
        let mono;
        let input = if self.channels == 1 {
            // El conversor de canales de rodio (2 → 1) se queda con el izquierdo y tira el derecho.
            mono = resample::downmix_stereo(samples);
            &mono[..]
        } else {
            samples
        };
        let mut buf = Vec::new();
        let rate = match &mut self.resampler {
            Some(r) => {
                r.process(input, &mut buf);
                self.rate
            }
            None => {
                buf.extend(input.iter().map(|&s| s as f32));
                SAMPLE_RATE
            }
        };
        self.push(buf, rate);
    }

    /// Saca lo que el remuestreador aún retenía (~1,8 ms) y lo deja listo para un flujo nuevo.
    fn flush(&mut self) {
        if let Some(r) = &mut self.resampler {
            let mut buf = Vec::new();
            r.flush(&mut buf);
            let rate = self.rate;
            self.push(buf, rate);
        }
    }

    fn push(&mut self, buf: Vec<f32>, rate: u32) {
        // Un paquete muy corto puede no completar aún ninguna muestra de salida.
        if !buf.is_empty() {
            // Con la cola vacía, rodio leería las primeras 512 muestras con el formato de su
            // relleno (ver `out_queue::SPAN_GUARD_SAMPLES`): que sean silencio y no música. Pasa
            // al abrir la salida y tras cada vaciado (buscar, «siguiente», entre canciones).
            let guard = out_queue::span_guard(self.sink.empty());
            if guard > 0 {
                self.clock.on_append(guard);
                let counter = self.clock.counter();
                self.sink.append(CountedBuffer::new(
                    self.src_channels(),
                    rate,
                    vec![0.0; guard],
                    counter,
                ));
            }
            self.clock.on_append(buf.len());
            let counter = self.clock.counter();
            self.sink
                .append(CountedBuffer::new(self.src_channels(), rate, buf, counter));
        }
    }
}

fn src_channels(device_channels: u16) -> u16 {
    if device_channels == 1 { 1 } else { NUM_CHANNELS as u16 }
}

/// Un paquete para rodio, como su `SamplesBuffer`, que además cuenta lo que el dispositivo le va
/// tomando (`QueueClock`): así se sabe cuánto de lo escrito aún no ha sonado.
struct CountedBuffer {
    data: Vec<f32>,
    pos: usize,
    channels: u16,
    rate: u32,
    counter: ConsumeCounter,
}

impl CountedBuffer {
    fn new(channels: u16, rate: u32, data: Vec<f32>, counter: ConsumeCounter) -> Self {
        Self {
            data,
            pos: 0,
            channels: channels.max(1),
            rate: rate.max(1),
            counter,
        }
    }
}

impl Iterator for CountedBuffer {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        let sample = *self.data.get(self.pos)?;
        self.pos += 1;
        self.counter.tick();
        if self.pos == self.data.len() {
            self.counter.publish();
        }
        Some(sample)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.data.len() - self.pos;
        (left, Some(left))
    }
}

impl rodio::Source for CountedBuffer {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        let frames = (self.data.len() / self.channels as usize) as f64;
        Some(Duration::from_secs_f64(frames / self.rate as f64))
    }
}

/// «44,1», «48», «88,2»: kHz como se escriben en español.
fn khz(hz: u32) -> String {
    let s = format!("{:.3}", hz as f64 / 1000.0);
    s.trim_end_matches('0').trim_end_matches('.').replace('.', ",")
}

/// Remuestreador de 44,1 kHz a la frecuencia del dispositivo, o `None` si no hace falta o si se
/// deja a rodio: NANOFY_RESAMPLER=rodio (para comparar a oído) o una relación de frecuencias que
/// pediría una tabla enorme. Deja anotado cómo suena la salida para `output_info`.
fn output_resampler(rate: u32, channels: u16) -> Option<Resampler> {
    let src_ch = if channels == 1 { 1 } else { NUM_CHANNELS as usize };
    let a_rodio = std::env::var("NANOFY_RESAMPLER")
        .is_ok_and(|v| v.trim().eq_ignore_ascii_case("rodio"));
    let resampler = if rate == SAMPLE_RATE || a_rodio {
        None
    } else {
        Resampler::new(SAMPLE_RATE, rate, src_ch)
    };
    let (de, a) = (khz(SAMPLE_RATE), khz(rate));
    let modo = if rate == SAMPLE_RATE {
        "sin remuestreo".to_string()
    } else if resampler.is_some() {
        format!("remuestreo sinc {de}→{a} kHz")
    } else if a_rodio {
        format!("remuestreo lineal de rodio {de}→{a} kHz (NANOFY_RESAMPLER=rodio)")
    } else {
        warn!(
            "salida de audio: {SAMPLE_RATE}→{rate} Hz no cabe en el remuestreador sinc; \
             convierte rodio"
        );
        format!("remuestreo lineal de rodio {de}→{a} kHz")
    };
    info!("salida de audio: {rate} Hz, {channels} canales, {modo}");
    OUTPUT_RATE.store(rate, Ordering::Relaxed);
    OUTPUT_CH.store(channels, Ordering::Relaxed);
    RESAMPLED.store(resampler.is_some(), Ordering::Relaxed);
    resampler
}

pub struct RodioSink {
    /// `None` hasta que hace falta sonar, y otra vez tras un rato en pausa (`release`): con el
    /// flujo abierto Windows da el dispositivo por ocupado aunque solo suene silencio, y unos
    /// auriculares Bluetooth con varias conexiones no ceden el canal al teléfono.
    out: Option<Output>,
    /// Dispositivo elegido en los ajustes; `None` = el predeterminado de Windows, al que se sigue
    /// si cambia (p. ej. al reconectar los auriculares).
    device: Option<String>,
    format: AudioFormat,
    playing: bool,
    last_reopen: Instant,
    default_checked: Instant,
    /// Rampa corta para lo próximo que se escriba: la cola se vació o se perdió (buscar, otra
    /// canción, salida soltada o reabierta) y lo que entra no continúa lo que sonaba.
    fade_in: Option<FadeIn>,
    /// Desde cuándo no se consigue abrir ninguna salida (`None`: la última vez se abrió). La
    /// espera de `open_output` cuenta desde aquí: si al preparar la salida mientras cargaba ya no
    /// había dispositivo, al ir a sonar solo se espera lo que falte, y con la salida ya perdida
    /// cada intento es uno solo (el aviso llega enseguida y no se bloquea el reproductor otra vez).
    missing_since: Option<Instant>,
}

fn list_formats(device: &cpal::Device) {
    match device.default_output_config() {
        Ok(cfg) => {
            debug!("  Default config:");
            debug!("    {cfg:?}");
        }
        Err(e) => {
            // Use loglevel debug, since even the output is only debug
            debug!("Error getting default rodio::Sink config: {e}");
        }
    };

    match device.supported_output_configs() {
        Ok(mut cfgs) => {
            if let Some(first) = cfgs.next() {
                debug!("  Available configs:");
                debug!("    {first:?}");
            } else {
                return;
            }

            for cfg in cfgs {
                debug!("    {cfg:?}");
            }
        }
        Err(e) => {
            debug!("Error getting supported rodio::Sink configs: {e}");
        }
    }
}

fn list_outputs(host: &cpal::Host) -> Result<(), cpal::DevicesError> {
    let mut default_device_name = None;

    if let Some(default_device) = host.default_output_device() {
        default_device_name = default_device.name().ok();
        println!(
            "Default Audio Device:\n  {}",
            default_device_name.as_deref().unwrap_or("[unknown name]")
        );

        list_formats(&default_device);

        println!("Other Available Audio Devices:");
    } else {
        warn!("No default device was found");
    }

    for device in host.output_devices()? {
        match device.name() {
            Ok(name) if Some(&name) == default_device_name.as_ref() => (),
            Ok(name) => {
                println!("  {name}");
                list_formats(&device);
            }
            Err(e) => {
                warn!("Cannot get device name: {e}");
                println!("   [unknown name]");
                list_formats(&device);
            }
        }
    }

    Ok(())
}

type OpenOutput = (rodio::Sink, rodio::OutputStream, Arc<AtomicBool>, String);

fn create_sink(
    host: &cpal::Host,
    device: Option<String>,
    format: AudioFormat,
) -> Result<OpenOutput, RodioError> {
    // Pruebas (`NANOFY_FAULT=no_output`): como si no hubiera ningún dispositivo de salida.
    if crate::core::fault::no_output() {
        return Err(RodioError::NoDeviceAvailable);
    }
    let cpal_device = match device.as_deref() {
        Some("?") => match list_outputs(host) {
            Ok(()) => exit(0),
            Err(e) => {
                error!("{e}");
                exit(1);
            }
        },
        Some(device_name) => {
            // Ignore devices for which getting name fails, or format doesn't match
            host.output_devices()?
                .find(|d| d.name().ok().is_some_and(|name| name == device_name)) // Ignore devices for which getting name fails
                .ok_or_else(|| RodioError::DeviceNotAvailable(device_name.to_string()))?
        }
        None => host
            .default_output_device()
            .ok_or(RodioError::NoDeviceAvailable)?,
    };

    let name = cpal_device.name().ok();
    info!(
        "Using audio device: {}",
        name.as_deref().unwrap_or("[unknown name]")
    );

    // First try native stereo 44.1 kHz playback, then fall back to the device default sample rate
    // (some devices only support 48 kHz: Output::append converts with our sinc resampler), then
    // fall back to whatever the default device config is (like mono).
    let default_config = cpal_device.default_output_config()?;
    let config = cpal_device
        .supported_output_configs()?
        .find(|c| c.channels() == NUM_CHANNELS as cpal::ChannelCount)
        .and_then(|c| {
            c.try_with_sample_rate(cpal::SampleRate(SAMPLE_RATE))
                .or_else(|| c.try_with_sample_rate(default_config.sample_rate()))
        })
        .unwrap_or(default_config);

    let sample_format = match format {
        AudioFormat::F64 => cpal::SampleFormat::F64,
        AudioFormat::F32 => cpal::SampleFormat::F32,
        AudioFormat::S32 => cpal::SampleFormat::I32,
        AudioFormat::S24 | AudioFormat::S24_3 => cpal::SampleFormat::I24,
        AudioFormat::S16 => cpal::SampleFormat::I16,
    };

    // Si el dispositivo se desconecta (Bluetooth apagado, USB fuera), cpal lo comunica por este
    // callback y `write` reabre la salida con el dispositivo predeterminado que haya entonces.
    let dead = Arc::new(AtomicBool::new(false));
    let on_error = {
        let dead = dead.clone();
        move |e: cpal::StreamError| {
            warn!("salida de audio: {e}; se reabrirá");
            dead.store(true, Ordering::Relaxed);
        }
    };
    let mut stream = match rodio::OutputStreamBuilder::default()
        .with_device(cpal_device.clone())
        .with_config(&config.config())
        .with_sample_format(sample_format)
        .with_error_callback(on_error.clone())
        .open_stream()
    {
        Ok(exact_stream) => exact_stream,
        Err(e) => {
            warn!("unable to create Rodio output, falling back to default: {e}");
            rodio::OutputStreamBuilder::from_device(cpal_device)?
                .with_error_callback(on_error)
                .open_stream_or_fallback()?
        }
    };

    // disable logging on stream drop
    stream.log_on_drop(false);

    let sink = rodio::Sink::connect_new(stream.mixer());
    Ok((sink, stream, dead, name.unwrap_or_default()))
}

pub fn open(host: cpal::Host, device: Option<String>, format: AudioFormat) -> RodioSink {
    info!(
        "Using Rodio sink with format {format:?} and cpal host: {}",
        host.id().name()
    );
    // El dispositivo se abre al empezar a sonar (`start`), no aquí.
    RodioSink {
        out: None,
        device,
        format,
        playing: false,
        last_reopen: Instant::now(),
        default_checked: Instant::now(),
        fade_in: None,
        missing_since: None,
    }
}

/// Cada cuánto se mira, mientras suena, si Windows cambió de dispositivo predeterminado.
const DEFAULT_CHECK_EVERY: Duration = Duration::from_secs(2);

impl RodioSink {
    /// Abre (o vuelve a abrir) la salida de audio en el dispositivo elegido o en el predeterminado
    /// actual. Insiste hasta `REOPEN_TIMEOUT` (contado desde el primer intento fallido, ver
    /// `missing_since`) y devuelve si lo consiguió; lo que había en cola se
    /// pierde (unas décimas de segundo). No espera más para no dejar colgado al reproductor: si
    /// el dispositivo sigue sin volver, el siguiente paquete vuelve a intentarlo.
    fn open_output(&mut self) -> bool {
        let patience = self
            .missing_since
            .map_or(REOPEN_TIMEOUT, |t| REOPEN_TIMEOUT.saturating_sub(t.elapsed()));
        self.open_output_within(patience)
    }

    /// `open_output` insistiendo como mucho `patience` (cero: un solo intento).
    fn open_output_within(&mut self, patience: Duration) -> bool {
        // Cerrar lo anterior antes: mientras un flujo muerto viva, cpal sigue avisando de su error.
        // Lo que tuviera en cola se pierde: lo siguiente entra con la rampa.
        if self.out.take().is_some() {
            self.fade_in = Some(FadeIn::new(SAMPLE_RATE));
        }
        let t0 = Instant::now();
        let mut avisado = false;
        loop {
            match create_sink(&cpal::default_host(), self.device.clone(), self.format) {
                Ok((sink, stream, dead, device_name)) => {
                    sink.set_volume(output_volume());
                    if self.playing {
                        sink.play();
                    } else {
                        sink.pause();
                    }
                    // El formato real del flujo, no el pedido: si cpal no aceptó la configuración
                    // exacta, rodio abrió la predeterminada del dispositivo.
                    let rate = stream.config().sample_rate();
                    let channels = stream.config().channel_count();
                    let resampler = output_resampler(rate, channels);
                    let clock_rate = if resampler.is_some() { rate } else { SAMPLE_RATE };
                    self.out = Some(Output {
                        sink,
                        stream,
                        dead,
                        device_name,
                        rate,
                        channels,
                        resampler,
                        clock: QueueClock::new(clock_rate, src_channels(channels)),
                    });
                    self.last_reopen = Instant::now();
                    self.default_checked = Instant::now();
                    self.missing_since = None;
                    debug!("salida de audio abierta en {:?}", t0.elapsed());
                    return true;
                }
                Err(e) => {
                    if !avisado {
                        warn!("no hay salida de audio disponible ({e}); esperando a que vuelva");
                        avisado = true;
                    }
                    if t0.elapsed() >= patience {
                        warn!("sigue sin haber salida de audio; se reintentará más adelante");
                        self.last_reopen = Instant::now();
                        self.missing_since.get_or_insert(t0);
                        return false;
                    }
                    thread::sleep(Duration::from_millis(250));
                }
            }
        }
    }

    /// La salida lista para sonar: se abre si no lo está o si su dispositivo desapareció, y se
    /// muda al predeterminado de Windows si ha cambiado (al reconectar los auriculares, Windows
    /// vuelve a elegirlos; sin esto se seguía sonando en el dispositivo anterior, o en ninguno).
    fn ensure_output(&mut self) -> bool {
        let stale = match &self.out {
            None => true,
            Some(out) if out.is_dead() => true,
            Some(out) if self.device.is_none() && self.default_checked.elapsed() >= DEFAULT_CHECK_EVERY => {
                self.default_checked = Instant::now();
                let current = cpal::default_host().default_output_device().and_then(|d| d.name().ok());
                match current {
                    Some(name) if name != out.device_name => {
                        info!("salida de audio: el dispositivo predeterminado pasa de «{}» a «{name}»", out.device_name);
                        true
                    }
                    _ => false,
                }
            }
            Some(_) => false,
        };
        if stale {
            self.open_output();
        }
        self.out.is_some()
    }

    /// Encola un paquete en la salida, con lo que quede de la rampa de entrada si la hay.
    fn append(&mut self, samples: &[f64]) {
        let Some(out) = &mut self.out else { return };
        match &mut self.fade_in {
            Some(fade) => {
                let mut faded = samples.to_vec();
                fade.apply(&mut faded, NUM_CHANNELS as usize);
                if fade.finished() {
                    self.fade_in = None;
                }
                out.append(&faded);
            }
            None => out.append(samples),
        }
    }
}

impl Sink for RodioSink {
    fn start(&mut self) -> SinkResult<()> {
        self.playing = true;
        // Al reanudar se mira ya el predeterminado: si cambió en pausa, se abre en el nuevo.
        self.default_checked = Instant::now() - DEFAULT_CHECK_EVERY;
        if !self.ensure_output() {
            // Ningún dispositivo tras insistir `REOPEN_TIMEOUT`: el reproductor deja la canción
            // en pausa en vez de darla por sonando en silencio (y Nanofy lo explica).
            self.playing = false;
            OUTPUT_OK.store(false, Ordering::Relaxed);
            return Err(RodioError::NoDeviceAvailable.into());
        }
        OUTPUT_OK.store(true, Ordering::Relaxed);
        if let Some(out) = &self.out {
            out.sink.set_volume(output_volume());
            out.sink.play();
        }
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.playing = false;
        if let Some(out) = &mut self.out {
            // Lo último que llegó sigue en la ventana del remuestreador: se encola antes de esperar
            // a que la cola se vacíe, y lo siguiente (otra posición, otra canción) empieza limpio.
            out.flush();
            if !out.is_dead() {
                // Con el dispositivo muerto la cola nunca se vacía: no esperar.
                let t0 = Instant::now();
                while !out.sink.empty() && t0.elapsed() < Duration::from_secs(2) && !out.is_dead() {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            out.sink.pause();
        }
        Ok(())
    }

    fn pause_now(&mut self) -> SinkResult<()> {
        self.playing = false;
        // Sin esperar a la cola: rodio deja de tomar de ella en su siguiente comprobación (5 ms) y
        // lo encolado, igual que lo que retiene el remuestreador, sigue ahí para reanudar.
        if let Some(out) = &self.out {
            out.sink.pause();
        }
        Ok(())
    }

    fn clear(&mut self) {
        if let Some(out) = &mut self.out {
            out.discard_queue();
        }
        self.fade_in = Some(FadeIn::new(SAMPLE_RATE));
    }

    fn prepare(&mut self) -> SinkResult<()> {
        let ready = matches!(&self.out, Some(out) if !out.is_dead());
        if !ready {
            // Un solo intento: si ahora no hay dispositivo, `start` insistirá al ir a sonar.
            let t0 = Instant::now();
            if self.open_output_within(Duration::ZERO) {
                debug!("salida de audio preparada en {:?}", t0.elapsed());
            }
        }
        Ok(())
    }

    fn queued_ms(&self) -> u32 {
        self.out.as_ref().map_or(0, |out| out.clock.queued_ms())
    }

    fn release(&mut self) {
        if !self.playing && self.out.take().is_some() {
            // Lo que quedara en cola se fue con ella; el reproductor vuelve a decodificar desde
            // lo oído al reanudar, y entra con la rampa.
            self.fade_in = Some(FadeIn::new(SAMPLE_RATE));
            debug!("salida de audio liberada (en pausa)");
        }
    }

    fn write(&mut self, packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
        // Se guarda el paquete en f64: la conversión a f32 la hace `Output::append` al
        // remuestrear, y si la salida se reabre el mismo paquete se vuelve a pasar por la nueva
        // (cuya frecuencia puede ser otra). Para F32 el `Converter` solo hacía un `as f32`.
        let samples = packet
            .samples()
            .map_err(|e| RodioError::Samples(e.to_string()))?;
        // Si llega audio es que el reproductor está sonando: que el sink nunca se quede en pausa.
        // Una reapertura en el momento justo lo dejaba mudo hasta reiniciar la aplicación.
        self.playing = true;
        if !self.ensure_output() {
            // El dispositivo desapareció y no hay otro: error, para que el reproductor pause en
            // vez de seguir decodificando hacia ninguna parte con la barra en «sonando».
            self.playing = false;
            OUTPUT_OK.store(false, Ordering::Relaxed);
            return Err(RodioError::NoDeviceAvailable.into());
        }
        if let Some(out) = &self.out {
            if out.sink.is_paused() {
                out.sink.play();
            }
            // Chunk sizes seem to be about 256 to 3000 ish items long.
            // Assuming they're on average 1628 then a half second buffer is:
            // 44100 elements --> about 27 chunks
            let v = output_volume();
            if (out.sink.volume() - v).abs() > 1e-6 {
                out.sink.set_volume(v);
                debug!("rodio: volumen de salida aplicado {v:.3}");
            }
        }
        self.append(samples);
        // El audio de una orden medida (`ttfs`) acaba de llegar a la salida: ahí se oye.
        crate::core::ttfs::on_output();

        // Espera a que la cola baje; si deja de bajar (el dispositivo dejó de consumir sin avisar)
        // o cpal ha avisado del fallo, se reabre la salida.
        let mut last_len = self.out.as_ref().map_or(0, |o| o.sink.len());
        let mut since = Instant::now();
        while let Some(out) = &self.out {
            if out.sink.len() <= 12 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
            let l = out.sink.len();
            if l < last_len {
                last_len = l;
                since = Instant::now();
                continue;
            }
            if !out.is_dead() && since.elapsed() < STALL_TIMEOUT {
                continue;
            }
            // Cada reapertura descarta la cola: encadenarlas deja la música muda aunque la salida
            // esté sana. Si se acaba de reabrir, se espera antes de volver a hacerlo.
            if self.last_reopen.elapsed() < REOPEN_COOLDOWN {
                continue;
            }
            warn!("la salida de audio no consume datos ({l} en cola); se reabre");
            if self.open_output() {
                // La cola se fue con el flujo anterior: reponer al menos el paquete de ahora,
                // pasado por el remuestreador de la salida nueva.
                self.append(samples);
            }
            break;
        }
        Ok(())
    }
}

impl RodioSink {
    #[allow(dead_code)]
    pub const NAME: &'static str = "rodio";
}
