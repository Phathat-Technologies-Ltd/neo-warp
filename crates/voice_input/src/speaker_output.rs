//! Plays synthesized speech on the default output device, one clip at a time, and stops at once
//! for barge-in.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use anyhow::Context as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use crate::live::LinearResampler;
use crate::voice_processing::system_aec;
use crate::voice_processing::{self, AudioBackend, OnFinished, OutputLease, open_with_fallback};

/// The speech gain the output ramps toward, as `f32` bits.
static TARGET_GAIN: AtomicU32 = AtomicU32::new(0x3f80_0000);
/// The gain applied to the last rendered sample, as `f32` bits.
static CURRENT_GAIN: AtomicU32 = AtomicU32::new(0x3f80_0000);
/// Gain change per output sample: a full swing takes about 20 ms at 48 kHz, short enough to feel
/// immediate and long enough not to click.
const GAIN_STEP: f32 = 1.0 / 960.0;

/// Scales rendered speech by the output gain, ramping toward [`SpeechOutput::set_gain`]'s target.
pub(crate) fn apply_playback_gain(out: &mut [f32]) {
    let target = f32::from_bits(TARGET_GAIN.load(Ordering::Relaxed));
    let mut gain = f32::from_bits(CURRENT_GAIN.load(Ordering::Relaxed));
    if gain == 1.0 && target == 1.0 {
        return;
    }
    for sample in out {
        gain = if gain < target {
            (gain + GAIN_STEP).min(target)
        } else {
            (gain - GAIN_STEP).max(target)
        };
        *sample *= gain;
    }
    CURRENT_GAIN.store(gain.to_bits(), Ordering::Relaxed);
}

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
        apply_playback_gain(out);
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
    /// The [`voice_processing::backend_generation`] the backend was chosen at.
    generation: AtomicU64,
    device_name: String,
    reports_headphones: bool,
}

impl SpeechOutput {
    /// Opens the default output device, on the echo-cancelling unit while
    /// [`voice_processing::set_voice_processing_wanted`] is on, the unit works, and system echo
    /// cancellation isn't running. The backend is chosen again before a clip plays whenever the
    /// audio backends changed since.
    pub fn open() -> anyhow::Result<Self> {
        let (backend, generation) = open_backend()?;
        let (device_name, reports_headphones) = default_output_description();
        Ok(Self {
            backend: Mutex::new(backend),
            generation: AtomicU64::new(generation),
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
        backend_kind(&self.backend.lock())
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
        self.reselect_if_changed(&mut backend);
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

    /// Moves to the backend the speaker should now use (for example, onto the echo-cancelling unit
    /// after system echo cancellation fell back) when the audio backends changed since it was
    /// chosen.
    fn reselect_if_changed(&self, backend: &mut Backend) {
        let generation = voice_processing::backend_generation();
        if self.generation.swap(generation, Ordering::AcqRel) == generation {
            return;
        }
        let desired = voice_processing::with_selector(|selector| {
            voice_processing::desired_output_backend(
                selector,
                voice_processing::is_supported(),
                system_aec::is_running(),
            )
        });
        if !needs_reselect(backend_kind(backend), desired) {
            return;
        }
        match open_backend() {
            Ok((reopened, generation)) => {
                *backend = reopened;
                self.generation.store(generation, Ordering::Release);
            }
            Err(error) => log::error!("Speech output: couldn't reopen the speaker: {error:#}"),
        }
    }

    /// Ramps the speech volume to `gain` (1.0 is full volume) over a few milliseconds.
    pub fn set_gain(&self, gain: f32) {
        TARGET_GAIN.store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
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

fn backend_kind(backend: &Backend) -> AudioBackend {
    match backend {
        Backend::Cpal(_) => AudioBackend::Cpal,
        Backend::VoiceProcessing(_) => AudioBackend::VoiceProcessing,
    }
}

/// Whether a speaker on `current` must reopen to play on `desired`. Speech plays through cpal for
/// system echo cancellation, so only a move onto or off the voice-processing unit reopens it.
fn needs_reselect(current: AudioBackend, desired: AudioBackend) -> bool {
    let on_unit = |backend| backend == AudioBackend::VoiceProcessing;
    on_unit(current) != on_unit(desired)
}

/// Opens the speaker on the backend the selector picks now, returning the backend generation it
/// was chosen at.
fn open_backend() -> anyhow::Result<(Backend, u64)> {
    let generation = voice_processing::backend_generation();
    let system_aec_running = system_aec::is_running();
    let (backend, kind) = voice_processing::with_selector(|selector| {
        open_with_fallback(
            selector,
            voice_processing::is_supported()
                && system_aec::output_backend(system_aec_running, AudioBackend::VoiceProcessing)
                    == AudioBackend::VoiceProcessing,
            || voice_processing::acquire_output().map(Backend::VoiceProcessing),
            || CpalOutput::open().map(Backend::Cpal),
        )
    })?;
    if kind == AudioBackend::VoiceProcessing {
        log::info!("Speech output: using the echo-cancelling voice-processing unit");
    } else if system_aec_running {
        log::info!("Speech output: plain path; system echo cancellation hears it through the tap");
    } else {
        log::info!("Speech output: plain path");
    }
    Ok((backend, generation))
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
