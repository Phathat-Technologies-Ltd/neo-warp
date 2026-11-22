//! Continuous microphone capture for hands-free input: 16 kHz mono PCM delivered in small chunks
//! as it is recorded, with nothing kept in memory or written to disk.

use anyhow::Context as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const TARGET_RATE: f64 = 16_000.0;

/// A chunk of captured audio.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveChunk {
    /// 16 kHz mono little-endian `i16` PCM.
    pub pcm: Vec<u8>,
    /// Root-mean-square level of the chunk in `[0, 1]`, for a level meter.
    pub level: f32,
}

/// An open microphone; dropping it closes the microphone.
pub struct LiveCapture {
    _stream: cpal::Stream,
}

impl LiveCapture {
    /// Opens the default input device and calls `on_chunk` from the audio thread for each chunk.
    pub fn start(mut on_chunk: impl FnMut(LiveChunk) + Send + 'static) -> anyhow::Result<Self> {
        let device = cpal::default_host()
            .default_input_device()
            .context("no default input device")?;
        let config = device
            .default_input_config()
            .context("failed to get the default input config")?;
        let channels = usize::from(config.channels().max(1));
        let mut resampler = LinearResampler::new(f64::from(config.sample_rate()), TARGET_RATE);
        let mut logged_error = false;
        let stream = device
            .build_input_stream(
                config.into(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let mono: Vec<f32> = data
                        .chunks_exact(channels)
                        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                        .collect();
                    let samples = resampler.process(&mono);
                    if !samples.is_empty() {
                        on_chunk(to_chunk(&samples));
                    }
                },
                move |error| {
                    if !logged_error {
                        logged_error = true;
                        log::error!("Hands-free microphone stream error: {error}");
                    }
                },
                None,
            )
            .context("failed to build the input stream")?;
        stream.play().context("failed to start the input stream")?;
        Ok(Self { _stream: stream })
    }
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
