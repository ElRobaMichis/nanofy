use std::process::exit;
use std::thread;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

use cpal::traits::{DeviceTrait, HostTrait};
use thiserror::Error;

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
/// Cuánto insistir en abrir la salida antes de devolver el control al reproductor.
const REOPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Salida de audio abierta: el sink de rodio y el flujo de cpal que lo alimenta.
struct Output {
    sink: rodio::Sink,
    _stream: rodio::OutputStream,
    /// cpal avisa por aquí de que el flujo murió (dispositivo desconectado). Uno por flujo: el
    /// del anterior sigue avisando mientras cpal lo cierra y daría por muerto al recién abierto.
    dead: Arc<AtomicBool>,
    /// Nombre del dispositivo en el que se abrió (para seguir al predeterminado de Windows).
    device_name: String,
}

impl Output {
    fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Relaxed)
    }
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
    // (some devices only support 48 kHz and Rodio will resample linearly), then fall back to
    // whatever the default device config is (like mono).
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
    }
}

/// Cada cuánto se mira, mientras suena, si Windows cambió de dispositivo predeterminado.
const DEFAULT_CHECK_EVERY: Duration = Duration::from_secs(2);

impl RodioSink {
    /// Abre (o vuelve a abrir) la salida de audio en el dispositivo elegido o en el predeterminado
    /// actual. Insiste hasta `REOPEN_TIMEOUT` y devuelve si lo consiguió; lo que había en cola se
    /// pierde (unas décimas de segundo). No espera más para no dejar colgado al reproductor: si
    /// el dispositivo sigue sin volver, el siguiente paquete vuelve a intentarlo.
    fn open_output(&mut self) -> bool {
        // Cerrar lo anterior antes: mientras un flujo muerto viva, cpal sigue avisando de su error.
        self.out = None;
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
                    self.out = Some(Output { sink, _stream: stream, dead, device_name });
                    self.last_reopen = Instant::now();
                    self.default_checked = Instant::now();
                    debug!("salida de audio abierta en {:?}", t0.elapsed());
                    return true;
                }
                Err(e) => {
                    if !avisado {
                        warn!("no hay salida de audio disponible ({e}); esperando a que vuelva");
                        avisado = true;
                    }
                    if t0.elapsed() >= REOPEN_TIMEOUT {
                        warn!("sigue sin haber salida de audio; se reintentará más adelante");
                        self.last_reopen = Instant::now();
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

    fn append(&self, samples: &[f32]) {
        if let Some(out) = &self.out {
            out.sink.append(rodio::buffer::SamplesBuffer::new(
                NUM_CHANNELS as cpal::ChannelCount,
                SAMPLE_RATE,
                samples,
            ));
        }
    }
}

impl Sink for RodioSink {
    fn start(&mut self) -> SinkResult<()> {
        self.playing = true;
        // Al reanudar se mira ya el predeterminado: si cambió en pausa, se abre en el nuevo.
        self.default_checked = Instant::now() - DEFAULT_CHECK_EVERY;
        if self.ensure_output() {
            if let Some(out) = &self.out {
                out.sink.set_volume(output_volume());
                out.sink.play();
            }
        }
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.playing = false;
        if let Some(out) = &self.out {
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

    fn release(&mut self) {
        if !self.playing && self.out.take().is_some() {
            debug!("salida de audio liberada (en pausa)");
        }
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet
            .samples()
            .map_err(|e| RodioError::Samples(e.to_string()))?;
        let samples_f32: &[f32] = &converter.f64_to_f32(samples);
        // Si llega audio es que el reproductor está sonando: que el sink nunca se quede en pausa.
        // Una reapertura en el momento justo lo dejaba mudo hasta reiniciar la aplicación.
        self.playing = true;
        if !self.ensure_output() {
            return Ok(());
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
        self.append(samples_f32);

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
                // La cola se fue con el flujo anterior: reponer al menos el paquete de ahora.
                self.append(samples_f32);
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
