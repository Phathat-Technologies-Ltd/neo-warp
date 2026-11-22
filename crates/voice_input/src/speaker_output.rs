//! Plays synthesized speech on the default output device, one clip at a time, and stops at once
//! for barge-in.

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::Context as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use crate::live::LinearResampler;

type OnFinished = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Shared {
    /// Mono samples at the device rate still to play.
    samples: VecDeque<f32>,
    on_finished: Option<OnFinished>,
}

impl Shared {
    /// Fills `out` (interleaved, `channels` per frame) and returns the callback to run when the
    /// clip has just ended.
    fn fill(&mut self, out: &mut [f32], channels: usize) -> Option<OnFinished> {
        for frame in out.chunks_mut(channels) {
            let sample = self.samples.pop_front().unwrap_or(0.0);
            frame.fill(sample);
        }
        if self.samples.is_empty() {
            self.on_finished.take()
        } else {
            None
        }
    }
}

/// An open output stream; dropping it closes the device.
pub struct SpeechOutput {
    _stream: cpal::Stream,
    shared: Arc<Mutex<Shared>>,
    device_rate: u32,
}

impl SpeechOutput {
    /// Opens the default output device.
    pub fn open() -> anyhow::Result<Self> {
        let device = cpal::default_host()
            .default_output_device()
            .context("no default output device")?;
        let config = device
            .default_output_config()
            .context("failed to get the default output config")?;
        if config.sample_format() != cpal::SampleFormat::F32 {
            anyhow::bail!(
                "unsupported speaker sample format {:?}",
                config.sample_format()
            );
        }
        let channels = usize::from(config.channels().max(1));
        let device_rate = config.sample_rate();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let callback_shared = shared.clone();
        let mut logged_error = false;
        let stream = device
            .build_output_stream(
                config.into(),
                move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let finished = callback_shared.lock().fill(out, channels);
                    if let Some(finished) = finished {
                        finished();
                    }
                },
                move |error| {
                    if !logged_error {
                        logged_error = true;
                        log::error!("Speech output stream error: {error}");
                    }
                },
                None,
            )
            .context("failed to build the output stream")?;
        stream.play().context("failed to start the output stream")?;
        Ok(Self {
            _stream: stream,
            shared,
            device_rate,
        })
    }

    /// Plays mono `samples` recorded at `sample_rate`, replacing anything playing, and calls
    /// `on_finished` from the audio thread when the clip ends. A stopped clip never calls it.
    pub fn play(
        &self,
        samples: &[f32],
        sample_rate: u32,
        on_finished: impl FnOnce() + Send + 'static,
    ) {
        let resampled = if sample_rate == self.device_rate {
            samples.to_vec()
        } else {
            LinearResampler::new(f64::from(sample_rate), f64::from(self.device_rate))
                .process(samples)
        };
        let mut shared = self.shared.lock();
        shared.samples = resampled.into();
        shared.on_finished = Some(Box::new(on_finished));
    }

    /// Stops playback at once.
    pub fn stop(&self) {
        let mut shared = self.shared.lock();
        shared.samples.clear();
        shared.on_finished = None;
    }
}

#[cfg(test)]
#[path = "speaker_output_tests.rs"]
mod tests;
