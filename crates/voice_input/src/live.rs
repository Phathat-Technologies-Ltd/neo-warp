//! Continuous microphone capture for hands-free input: 16 kHz mono PCM delivered in small chunks
//! as it is recorded, with nothing kept in memory or written to disk.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};

use anyhow::Context as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::recovery::{Heartbeat, RecoveryTiming, Signal, StreamFault, supervise};

const TARGET_RATE: f64 = 16_000.0;
/// Audio callbacks between progress logs (roughly ten seconds at typical buffer sizes).
const CALLBACKS_PER_LOG: u64 = 1000;

/// A chunk of captured audio.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveChunk {
    /// 16 kHz mono little-endian `i16` PCM.
    pub pcm: Vec<u8>,
    /// Root-mean-square level of the chunk in `[0, 1]`, for a level meter.
    pub level: f32,
}

/// An open microphone; dropping it closes the microphone. The stream lives on a supervisor thread
/// that rebuilds it when it fails or stalls.
pub struct LiveCapture {
    signal_tx: mpsc::Sender<Signal>,
}

impl Drop for LiveCapture {
    fn drop(&mut self) {
        let _ = self.signal_tx.send(Signal::Stop);
    }
}

impl LiveCapture {
    /// Opens the default input device and calls `on_chunk` from the audio thread for each chunk.
    /// `on_failed` is called once if the microphone breaks and can't be reopened.
    pub fn start(
        on_chunk: impl FnMut(LiveChunk) + Send + 'static,
        on_failed: impl FnOnce(String) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (signal_tx, signal_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let on_chunk = Arc::new(Mutex::new(on_chunk));
        let fault_tx = signal_tx.clone();
        std::thread::Builder::new()
            .name("hands-free-capture".to_owned())
            .spawn(move || {
                supervise(
                    |heartbeat| open_stream(on_chunk.clone(), heartbeat, fault_tx.clone()),
                    signal_rx,
                    RecoveryTiming::default(),
                    move |result| {
                        let _ = started_tx.send(result);
                    },
                    on_failed,
                );
            })
            .context("failed to start the capture thread")?;
        started_rx.recv().context("the capture thread exited")??;
        Ok(Self { signal_tx })
    }
}

/// Opens and starts an input stream on the default device.
fn open_stream<F>(
    on_chunk: Arc<Mutex<F>>,
    heartbeat: Heartbeat,
    fault_tx: mpsc::Sender<Signal>,
) -> anyhow::Result<cpal::Stream>
where
    F: FnMut(LiveChunk) + Send + 'static,
{
    let device = cpal::default_host()
        .default_input_device()
        .context("no default input device")?;
    let config = device
        .default_input_config()
        .context("failed to get the default input config")?;
    let channels = usize::from(config.channels().max(1));
    log::info!(
        "Hands-free microphone: opening {:?} at {} Hz, {channels} channel(s), {:?}, resampling to \
         {TARGET_RATE} Hz",
        device
            .description()
            .map(|description| description.name().to_owned())
            .unwrap_or_default(),
        config.sample_rate(),
        config.sample_format()
    );
    let mut resampler = LinearResampler::new(f64::from(config.sample_rate()), TARGET_RATE);
    let mut callbacks: u64 = 0;
    let mut peak_level: f32 = 0.0;
    let on_mono = move |mono: Vec<f32>| {
        heartbeat.fetch_add(1, Ordering::Relaxed);
        let samples = resampler.process(&mono);
        callbacks += 1;
        if callbacks == 1 {
            log::info!(
                "Hands-free microphone: first audio callback ({} frames in, {} samples out)",
                mono.len(),
                samples.len()
            );
        }
        if samples.is_empty() {
            return;
        }
        let chunk = to_chunk(&samples);
        peak_level = peak_level.max(chunk.level);
        if callbacks.is_multiple_of(CALLBACKS_PER_LOG) {
            if peak_level == 0.0 {
                // macOS delivers zeros instead of failing when the app lacks microphone access.
                log::warn!(
                    "Hands-free microphone: {callbacks} audio callbacks of pure silence; check \
                     microphone access and the input device"
                );
            }
            log::info!(
                "Hands-free microphone: {callbacks} audio callbacks, peak level {peak_level:.3} \
                 since the last report"
            );
            peak_level = 0.0;
        }
        if let Ok(mut on_chunk) = on_chunk.lock() {
            on_chunk(chunk);
        }
    };
    let sample_format = config.sample_format();
    let config: cpal::StreamConfig = config.into();
    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            build_stream::<f32>(&device, &config, channels, on_mono, fault_tx)
        }
        cpal::SampleFormat::I16 => {
            build_stream::<i16>(&device, &config, channels, on_mono, fault_tx)
        }
        cpal::SampleFormat::U16 => {
            build_stream::<u16>(&device, &config, channels, on_mono, fault_tx)
        }
        cpal::SampleFormat::I32 => {
            build_stream::<i32>(&device, &config, channels, on_mono, fault_tx)
        }
        other => anyhow::bail!("unsupported microphone sample format {other:?}"),
    }?;
    stream.play().context("failed to start the input stream")?;
    Ok(stream)
}

/// Builds an input stream for samples of type `T`, downmixing each callback to mono `f32`. Stream
/// errors are reported to the supervisor.
fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut on_mono: impl FnMut(Vec<f32>) + Send + 'static,
    fault_tx: mpsc::Sender<Signal>,
) -> anyhow::Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let mut logged_glitch = false;
    device
        .build_input_stream(
            *config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                on_mono(downmix(data, channels));
            },
            move |error: cpal::Error| {
                let fault = StreamFault::classify(error.kind());
                match fault {
                    // Glitches can repeat many times a second; one log line is enough.
                    StreamFault::Transient if logged_glitch => {}
                    StreamFault::Transient => {
                        logged_glitch = true;
                        log::warn!("Hands-free microphone glitch, audio dropped: {error}");
                    }
                    StreamFault::Fatal => {
                        log::error!("Hands-free microphone stream error: {error}");
                    }
                }
                let _ = fault_tx.send(Signal::Fault(fault));
            },
            None,
        )
        .context("failed to build the input stream")
}

/// Averages interleaved frames of `channels` samples into mono `f32`.
pub fn downmix<T>(data: &[T], channels: usize) -> Vec<f32>
where
    T: cpal::Sample,
    f32: cpal::FromSample<T>,
{
    use cpal::Sample as _;
    data.chunks_exact(channels)
        .map(|frame| frame.iter().map(|&s| f32::from_sample(s)).sum::<f32>() / channels as f32)
        .collect()
}

fn to_chunk(samples: &[f32]) -> LiveChunk {
    let mut pcm = Vec::with_capacity(samples.len() * 2);
    let mut energy = 0.0;
    for &sample in samples {
        let sample = sample.clamp(-1.0, 1.0);
        energy += sample * sample;
        pcm.extend_from_slice(&((sample * 32767.0) as i16).to_le_bytes());
    }
    LiveChunk {
        pcm,
        level: (energy / samples.len() as f32).sqrt(),
    }
}

/// Resamples a continuous signal by linear interpolation, carrying position across calls. Speech
/// recognition at 16 kHz tolerates its mild aliasing.
#[derive(Debug, Clone)]
pub struct LinearResampler {
    step: f64,
    /// Position of the next output sample, where 0 is the last input sample of the previous call.
    position: f64,
    previous: Option<f32>,
}

impl LinearResampler {
    pub fn new(from_rate: f64, to_rate: f64) -> Self {
        Self {
            step: from_rate / to_rate,
            position: 0.0,
            previous: None,
        }
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let Some(&last) = input.last() else {
            return Vec::new();
        };
        let mut work = Vec::with_capacity(input.len() + 1);
        work.extend(self.previous);
        work.extend_from_slice(input);
        let mut output = Vec::with_capacity((work.len() as f64 / self.step) as usize + 1);
        while (self.position.floor() as usize) + 1 < work.len() {
            let index = self.position.floor() as usize;
            let fraction = (self.position - index as f64) as f32;
            output.push(work[index] + (work[index + 1] - work[index]) * fraction);
            self.position += self.step;
        }
        self.position -= (work.len() - 1) as f64;
        self.previous = Some(last);
        output
    }
}

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
