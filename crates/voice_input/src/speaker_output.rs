//! Plays synthesized speech on the default output device, one clip at a time, and stops at once
//! for barge-in.

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::Context as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use crate::live::LinearResampler;
use crate::voice_processing::system_aec;
use crate::voice_processing::{self, AudioBackend, OnFinished, OutputLease, open_with_fallback};

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

/// A plain cpal output stream.
struct CpalOutput {
    _stream: cpal::Stream,
    shared: Arc<Mutex<Shared>>,
    device_rate: u32,
}

impl CpalOutput {
    fn open() -> anyhow::Result<Self> {
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
}

enum Backend {
    Cpal(CpalOutput),
    VoiceProcessing(OutputLease),
}

/// An open speaker; dropping it closes the device.
pub struct SpeechOutput {
    backend: Mutex<Backend>,
    device_name: String,
    reports_headphones: bool,
}

impl SpeechOutput {
    /// Opens the default output device, on the echo-cancelling unit while
    /// [`voice_processing::set_voice_processing_wanted`] is on and the unit works.
    pub fn open() -> anyhow::Result<Self> {
        let system_aec_running = system_aec::is_running();
        let (backend, kind) = voice_processing::with_selector(|selector| {
            open_with_fallback(
                selector,
                voice_processing::is_supported()
                    && system_aec::output_backend(
                        system_aec_running,
                        AudioBackend::VoiceProcessing,
                    ) == AudioBackend::VoiceProcessing,
                || voice_processing::acquire_output().map(Backend::VoiceProcessing),
                || CpalOutput::open().map(Backend::Cpal),
            )
        })?;
        if kind == AudioBackend::VoiceProcessing {
            log::info!("Speech output: using the echo-cancelling voice-processing unit");
        } else if system_aec_running {
            log::info!(
                "Speech output: plain path; system echo cancellation hears it through the tap"
            );
        }
        let (device_name, reports_headphones) = default_output_description();
        Ok(Self {
            backend: Mutex::new(backend),
            device_name,
            reports_headphones,
        })
    }

    /// The name of the output device, such as "MacBook Pro Speakers".
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Whether the system reports the output device as headphones or a headset.
    pub fn reports_headphones(&self) -> bool {
        self.reports_headphones
    }

    /// Which backend plays the speech.
    pub fn backend(&self) -> AudioBackend {
        match &*self.backend.lock() {
            Backend::Cpal(_) => AudioBackend::Cpal,
            Backend::VoiceProcessing(_) => AudioBackend::VoiceProcessing,
        }
    }

    /// Plays mono `samples` recorded at `sample_rate`, replacing anything playing, and calls
    /// `on_finished` from the audio thread when the clip ends. A stopped clip never calls it.
    pub fn play(
        &self,
        samples: &[f32],
        sample_rate: u32,
        on_finished: impl FnOnce() + Send + 'static,
    ) {
        let mut backend = self.backend.lock();
        // A unit that stopped (say, it couldn't restart after a device change) would never finish
        // the clip, so speech moves to the plain path.
        if let Backend::VoiceProcessing(lease) = &*backend
            && !lease.is_running()
        {
            match CpalOutput::open() {
                Ok(output) => {
                    log::warn!("Speech output: echo-cancelling unit stopped, using the plain path");
                    *backend = Backend::Cpal(output);
                }
                Err(error) => log::error!("Speech output: couldn't reopen the speaker: {error:#}"),
            }
        }
        match &*backend {
            Backend::Cpal(output) => {
                let resampled = resample(samples, sample_rate, output.device_rate);
                let mut shared = output.shared.lock();
                shared.samples = resampled.into();
                shared.on_finished = Some(Box::new(on_finished));
            }
            Backend::VoiceProcessing(lease) => {
                let resampled = resample(samples, sample_rate, lease.output_rate());
                lease.play(&resampled, Box::new(on_finished));
            }
        }
    }

    /// Stops playback at once.
    pub fn stop(&self) {
        match &*self.backend.lock() {
            Backend::Cpal(output) => {
                let mut shared = output.shared.lock();
                shared.samples.clear();
                shared.on_finished = None;
            }
            Backend::VoiceProcessing(lease) => lease.stop(),
        }
    }
}

fn resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate {
        samples.to_vec()
    } else {
        LinearResampler::new(f64::from(from_rate), f64::from(to_rate)).process(samples)
    }
}

/// The default output device's name and whether it reports itself as headphones.
fn default_output_description() -> (String, bool) {
    let Some(description) = cpal::default_host()
        .default_output_device()
        .and_then(|device| device.description().ok())
    else {
        return (String::new(), false);
    };
    let headphones = matches!(
        description.device_type(),
        cpal::DeviceType::Headphones
            | cpal::DeviceType::Headset
            | cpal::DeviceType::Earpiece
            | cpal::DeviceType::HearingAid
    );
    (description.name().to_owned(), headphones)
}

#[cfg(test)]
#[path = "speaker_output_tests.rs"]
mod tests;
