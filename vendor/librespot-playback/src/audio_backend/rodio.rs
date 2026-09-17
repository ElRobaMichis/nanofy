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

pub struct RodioSink {
    rodio_sink: rodio::Sink,
    _stream: rodio::OutputStream,
    /// Para reabrir la salida si el dispositivo desaparece (auriculares Bluetooth, USB…).
    device: Option<String>,
    format: AudioFormat,
    /// cpal avisa por aquí de que el flujo murió (dispositivo desconectado).
    dead: Arc<AtomicBool>,
    playing: bool,
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

fn create_sink(
    host: &cpal::Host,
    device: Option<String>,
    format: AudioFormat,
    dead: Arc<AtomicBool>,
) -> Result<(rodio::Sink, rodio::OutputStream), RodioError> {
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
    Ok((sink, stream))
}

pub fn open(host: cpal::Host, device: Option<String>, format: AudioFormat) -> RodioSink {
    info!(
        "Using Rodio sink with format {format:?} and cpal host: {}",
        host.id().name()
    );

    let dead = Arc::new(AtomicBool::new(false));
    let (sink, stream) = create_sink(&host, device.clone(), format, dead.clone()).unwrap();

    debug!("Rodio sink was created");
    RodioSink {
        rodio_sink: sink,
        _stream: stream,
        device,
        format,
        dead,
        playing: false,
    }
}

impl RodioSink {
    /// Vuelve a abrir la salida de audio (dispositivo predeterminado actual). Reintenta mientras
    /// no haya ninguno; lo que había en cola se pierde (unas décimas de segundo).
    fn reopen(&mut self) {
        for intento in 1..=120 {
            self.dead.store(false, Ordering::Relaxed);
            match create_sink(&cpal::default_host(), self.device.clone(), self.format, self.dead.clone()) {
                Ok((sink, stream)) => {
                    self.rodio_sink = sink;
                    self._stream = stream;
                    self.rodio_sink.set_volume(output_volume());
                    if self.playing {
                        self.rodio_sink.play();
                    } else {
                        self.rodio_sink.pause();
                    }
                    info!("salida de audio reabierta (intento {intento})");
                    return;
                }
                Err(e) => {
                    if intento == 1 {
                        warn!("no hay salida de audio disponible ({e}); esperando a que vuelva");
                    }
                    thread::sleep(Duration::from_millis(500));
                }
            }
        }
        warn!("sin salida de audio tras 60 s; se seguirá intentando con el siguiente paquete");
    }
}

impl Sink for RodioSink {
    fn start(&mut self) -> SinkResult<()> {
        self.playing = true;
        if self.dead.load(Ordering::Relaxed) {
            self.reopen();
        }
        self.rodio_sink.set_volume(output_volume());
        self.rodio_sink.play();
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.playing = false;
        if !self.dead.load(Ordering::Relaxed) {
            // Con el dispositivo muerto la cola nunca se vacía: no esperar.
            let t0 = Instant::now();
            while !self.rodio_sink.empty() && t0.elapsed() < Duration::from_secs(2) && !self.dead.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(10));
            }
        }
        self.rodio_sink.pause();
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet
            .samples()
            .map_err(|e| RodioError::Samples(e.to_string()))?;
        let samples_f32: &[f32] = &converter.f64_to_f32(samples);
        let source = rodio::buffer::SamplesBuffer::new(
            NUM_CHANNELS as cpal::ChannelCount,
            SAMPLE_RATE,
            samples_f32,
        );
        if self.dead.load(Ordering::Relaxed) {
            self.reopen();
        }
        self.rodio_sink.append(source);

        // Chunk sizes seem to be about 256 to 3000 ish items long.
        // Assuming they're on average 1628 then a half second buffer is:
        // 44100 elements --> about 27 chunks
        let v = output_volume();
        if (self.rodio_sink.volume() - v).abs() > 1e-6 {
            self.rodio_sink.set_volume(v);
            debug!("rodio: volumen de salida aplicado {v:.3}");
        }
        // Espera a que la cola baje; si no baja en 2 s (el dispositivo dejó de consumir sin avisar)
        // o cpal ha avisado del fallo, se reabre la salida.
        let mut last_len = self.rodio_sink.len();
        let mut since = Instant::now();
        while self.rodio_sink.len() > 12 {
            thread::sleep(Duration::from_millis(10));
            let l = self.rodio_sink.len();
            if l < last_len {
                last_len = l;
                since = Instant::now();
            } else if self.dead.load(Ordering::Relaxed) || since.elapsed() > Duration::from_secs(2) {
                warn!("la salida de audio no consume datos; se reabre");
                self.reopen();
                break;
            }
        }
        Ok(())
    }
}

impl RodioSink {
    #[allow(dead_code)]
    pub const NAME: &'static str = "rodio";
}
