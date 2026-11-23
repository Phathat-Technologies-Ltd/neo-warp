//! The voice-processing I/O unit (`kAudioUnitSubType_VoiceProcessingIO`): one unit owns the default
//! microphone (bus 1) and speaker (bus 0), so Apple's echo canceller can subtract the speech it
//! plays from what the microphone hears. It also applies noise suppression and makes the macOS Mic
//! Modes (such as Voice Isolation) available.
//!
//! The unit runs while any microphone or speaker lease is held. Its callbacks run on the Core Audio
//! I/O thread and never block: speech arrives through a lock-free ring, and microphone audio goes
//! to the capture sink only if its lock is free.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use anyhow::anyhow;
use coreaudio::audio_unit::audio_format::LinearPcmFlags;
use coreaudio::audio_unit::render_callback::{self, data};
use coreaudio::audio_unit::{AudioUnit, Element, IOType, SampleFormat, Scope, StreamFormat};
use objc2_audio_toolbox::{
    AURenderCallbackStruct, AUVoiceIOOtherAudioDuckingConfiguration,
    AUVoiceIOOtherAudioDuckingLevel, AudioUnitRender, AudioUnitRenderActionFlags,
    kAUVoiceIOProperty_BypassVoiceProcessing, kAUVoiceIOProperty_OtherAudioDuckingConfiguration,
    kAudioOutputUnitProperty_EnableIO, kAudioOutputUnitProperty_SetInputCallback,
};
use objc2_core_audio::{
    AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioTimeStamp};
use parking_lot::Mutex;

use super::{
    CAPTURE_RATE, CaptureSink, ECHO_CANCELLING, OnFinished, PLAYBACK_RATE, PlaybackQueue,
    reopen_captures,
};
use crate::live::LinearResampler;

/// Speech the speaker side can hold: about 43 s at 24 kHz, far longer than one sentence.
const PLAYBACK_CAPACITY: usize = 1 << 20;

type Args = render_callback::Args<data::NonInterleaved<f32>>;

struct Shared {
    playback: PlaybackQueue,
    sink: Mutex<Option<CaptureSink>>,
    running: AtomicBool,
    output_rate: AtomicU32,
}

/// Core Audio units may be stopped and disposed from any thread. The unit is disposed before the
/// microphone state its input callback points at.
#[allow(dead_code)]
struct RunningUnit(AudioUnit, Box<MicState>);
unsafe impl Send for RunningUnit {}

struct Engine {
    shared: Arc<Shared>,
    unit: Option<RunningUnit>,
    captures: usize,
    outputs: usize,
}

impl Engine {
    fn ensure_running(&mut self) -> anyhow::Result<()> {
        if NEEDS_RESTART.swap(false, Ordering::Relaxed) && self.unit.is_some() {
            log::info!("Echo-cancelling audio: default device changed, restarting the unit");
            self.unit = None;
            self.shared.running.store(false, Ordering::Relaxed);
        }
        if self.unit.is_none() {
            let (unit, output_rate) = start_unit(&self.shared)?;
            self.shared
                .output_rate
                .store(output_rate, Ordering::Relaxed);
            self.shared.running.store(true, Ordering::Relaxed);
            self.unit = Some(unit);
        }
        Ok(())
    }

    fn update_echo_flag(&self) {
        let cancelling = self.unit.is_some() && self.captures > 0 && self.outputs > 0;
        if ECHO_CANCELLING.swap(cancelling, Ordering::Relaxed) != cancelling {
            log::info!(
                "Echo-cancelling audio: echo cancellation {}",
                if cancelling { "active" } else { "inactive" }
            );
            super::notify_backend_changed();
        }
    }
}

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);
/// Set when the default microphone or speaker changes; the unit is rebuilt on its next use.
static NEEDS_RESTART: AtomicBool = AtomicBool::new(false);
static LISTEN_FOR_ROUTE_CHANGES: Once = Once::new();
/// Turns the echo canceller off in units started from now on, for measuring the raw echo.
static PROBE_BYPASS: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
enum LeaseKind {
    Capture,
    Output,
}

fn acquire(kind: LeaseKind) -> anyhow::Result<Arc<Shared>> {
    LISTEN_FOR_ROUTE_CHANGES.call_once(listen_for_route_changes);
    let mut guard = ENGINE.lock();
    let engine = guard.get_or_insert_with(|| Engine {
        shared: Arc::new(Shared {
            playback: PlaybackQueue::new(PLAYBACK_CAPACITY),
            sink: Mutex::new(None),
            running: AtomicBool::new(false),
            output_rate: AtomicU32::new(PLAYBACK_RATE as u32),
        }),
        unit: None,
        captures: 0,
        outputs: 0,
    });
    if let Err(error) = engine.ensure_running() {
        if engine.captures == 0 && engine.outputs == 0 {
            *guard = None;
        } else {
            engine.update_echo_flag();
        }
        return Err(error);
    }
    match kind {
        LeaseKind::Capture => engine.captures += 1,
        LeaseKind::Output => engine.outputs += 1,
    }
    engine.update_echo_flag();
    Ok(engine.shared.clone())
}

fn release(kind: LeaseKind) {
    let mut guard = ENGINE.lock();
    let Some(engine) = guard.as_mut() else {
        return;
    };
    match kind {
        LeaseKind::Capture => engine.captures = engine.captures.saturating_sub(1),
        LeaseKind::Output => engine.outputs = engine.outputs.saturating_sub(1),
    }
    if engine.captures == 0 && engine.outputs == 0 {
        // Dropping the unit waits for its callbacks to finish; they never take this lock.
        *guard = None;
        if ECHO_CANCELLING.swap(false, Ordering::Relaxed) {
            log::info!("Echo-cancelling audio: echo cancellation inactive");
            super::notify_backend_changed();
        }
    } else {
        engine.update_echo_flag();
    }
}

/// The microphone side of the unit; dropping it detaches the sink.
pub(crate) struct CaptureLease {
    shared: Arc<Shared>,
}

impl Drop for CaptureLease {
    fn drop(&mut self) {
        *self.shared.sink.lock() = None;
        release(LeaseKind::Capture);
    }
}

/// Starts the unit if needed and sends its microphone audio to `sink`, replacing any earlier sink.
pub(crate) fn acquire_capture(sink: CaptureSink) -> anyhow::Result<CaptureLease> {
    let shared = acquire(LeaseKind::Capture)?;
    *shared.sink.lock() = Some(sink);
    Ok(CaptureLease { shared })
}

/// The speaker side of the unit.
pub(crate) struct OutputLease {
    shared: Arc<Shared>,
}

impl Drop for OutputLease {
    fn drop(&mut self) {
        self.shared.playback.stop();
        release(LeaseKind::Output);
    }
}

impl OutputLease {
    pub(crate) fn output_rate(&self) -> u32 {
        self.shared.output_rate.load(Ordering::Relaxed)
    }

    /// Whether the unit is running, restarting it first if the default device changed.
    pub(crate) fn is_running(&self) -> bool {
        if NEEDS_RESTART.load(Ordering::Relaxed)
            && let Some(engine) = ENGINE.lock().as_mut()
        {
            if let Err(error) = engine.ensure_running() {
                log::warn!("Echo-cancelling audio: couldn't restart the unit: {error:#}");
            }
            engine.update_echo_flag();
        }
        self.shared.running.load(Ordering::Relaxed)
    }

    pub(crate) fn play(&self, samples: &[f32], on_finished: OnFinished) {
        self.shared.playback.play(samples, on_finished);
    }

    pub(crate) fn stop(&self) {
        self.shared.playback.stop();
    }
}

pub(crate) fn acquire_output() -> anyhow::Result<OutputLease> {
    Ok(OutputLease {
        shared: acquire(LeaseKind::Output)?,
    })
}

fn mono_format(sample_rate: f64) -> StreamFormat {
    StreamFormat {
        sample_rate,
        sample_format: SampleFormat::F32,
        flags: LinearPcmFlags::IS_FLOAT
            | LinearPcmFlags::IS_PACKED
            | LinearPcmFlags::IS_NON_INTERLEAVED,
        channels: 1,
    }
}

fn context(what: &'static str) -> impl FnOnce(coreaudio::Error) -> anyhow::Error {
    move |error| anyhow!("failed to {what}: {error:?}")
}

/// Creates, configures and starts the unit, returning it with its speaker-side sample rate.
fn start_unit(shared: &Arc<Shared>) -> anyhow::Result<(RunningUnit, u32)> {
    let mut unit = AudioUnit::new_uninitialized(IOType::VoiceProcessingIO)
        .map_err(context("create the voice-processing unit"))?;
    let enable: u32 = 1;
    unit.set_property(
        kAudioOutputUnitProperty_EnableIO,
        Scope::Input,
        Element::Input,
        Some(&enable),
    )
    .map_err(context("enable the microphone"))?;
    unit.set_property(
        kAudioOutputUnitProperty_EnableIO,
        Scope::Output,
        Element::Output,
        Some(&enable),
    )
    .map_err(context("enable the speaker"))?;
    // Both sides must share one rate: with the microphone at 16 kHz and the speaker at 24 kHz,
    // initialization fails with kAudioUnitErr_FailedInitialization. Microphone audio is resampled
    // to `CAPTURE_RATE` in the input callback.
    unit.set_stream_format(mono_format(PLAYBACK_RATE), Scope::Output, Element::Input)
        .map_err(context("set the microphone format"))?;
    unit.set_stream_format(mono_format(PLAYBACK_RATE), Scope::Input, Element::Output)
        .map_err(context("set the speaker format"))?;
    // The unit ducks other apps' audio by default; keep that to a minimum. Older macOS versions
    // lack the property, which only means default ducking.
    let ducking = AUVoiceIOOtherAudioDuckingConfiguration {
        mEnableAdvancedDucking: 1,
        mDuckingLevel: AUVoiceIOOtherAudioDuckingLevel::Min,
    };
    if let Err(error) = unit.set_property(
        kAUVoiceIOProperty_OtherAudioDuckingConfiguration,
        Scope::Global,
        Element::Output,
        Some(&ducking),
    ) {
        log::debug!("Echo-cancelling audio: couldn't reduce ducking: {error:?}");
    }

    if PROBE_BYPASS.load(Ordering::Relaxed) {
        unit.set_property(
            kAUVoiceIOProperty_BypassVoiceProcessing,
            Scope::Global,
            Element::Output,
            Some(&enable),
        )
        .map_err(context("bypass voice processing"))?;
    }

    let input_rate = unit
        .input_stream_format()
        .map_err(context("read the microphone format"))?
        .sample_rate;
    let output_rate = unit
        .output_stream_format()
        .map_err(context("read the speaker format"))?
        .sample_rate;
    log::info!(
        "Echo-cancelling audio: starting the voice-processing unit (microphone {input_rate} Hz, \
         speaker {output_rate} Hz)"
    );

    let mut mic = Box::new(MicState {
        unit: *unit.as_ref(),
        buffer: vec![0.0; MAX_INPUT_FRAMES],
        resampler: (input_rate != CAPTURE_RATE)
            .then(|| LinearResampler::new(input_rate, CAPTURE_RATE)),
        shared: shared.clone(),
    });
    let callback = AURenderCallbackStruct {
        inputProc: Some(on_microphone_input),
        inputProcRefCon: (&raw mut *mic).cast(),
    };
    unit.set_property(
        kAudioOutputUnitProperty_SetInputCallback,
        Scope::Global,
        Element::Output,
        Some(&callback),
    )
    .map_err(context("install the microphone callback"))?;
    let render_shared = shared.clone();
    unit.set_render_callback(move |mut args: Args| {
        if let Some(channel) = args.data.channels_mut().next() {
            render_shared.playback.render(channel);
        }
        Ok(())
    })
    .map_err(context("install the speaker callback"))?;
    unit.initialize()
        .map_err(context("initialize the voice-processing unit"))?;
    unit.start()
        .map_err(context("start the voice-processing unit"))?;
    Ok((RunningUnit(unit, mic), output_rate.round() as u32))
}

/// Largest microphone callback the unit is expected to deliver; bigger ones are dropped.
const MAX_INPUT_FRAMES: usize = 8192;

/// What the microphone callback needs. coreaudio-rs's input callback isn't used because it reads
/// the unit's stream format on the I/O thread whenever the callback size differs from the device
/// buffer size, which is always the case here, and that read deadlocks against `AudioOutputUnitStart`.
struct MicState {
    unit: objc2_audio_toolbox::AudioUnit,
    buffer: Vec<f32>,
    resampler: Option<LinearResampler>,
    shared: Arc<Shared>,
}

unsafe extern "C-unwind" fn on_microphone_input(
    ref_con: NonNull<c_void>,
    action_flags: NonNull<AudioUnitRenderActionFlags>,
    time_stamp: NonNull<AudioTimeStamp>,
    bus: u32,
    frames: u32,
    _data: *mut AudioBufferList,
) -> i32 {
    // SAFETY: `ref_con` is the `MicState` owned by the `RunningUnit`, which disposes the unit (and
    // with it this callback) before dropping the state; the I/O thread is its only user.
    let mic = unsafe { ref_con.cast::<MicState>().as_mut() };
    let frames_len = frames as usize;
    if frames_len > mic.buffer.len() {
        return 0;
    }
    let mut list = AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [AudioBuffer {
            mNumberChannels: 1,
            mDataByteSize: (frames_len * size_of::<f32>()) as u32,
            mData: mic.buffer.as_mut_ptr().cast(),
        }],
    };
    // SAFETY: the unit is alive while its callback runs and `list` points at `frames` samples.
    let status = unsafe {
        AudioUnitRender(
            mic.unit,
            action_flags.as_ptr(),
            time_stamp,
            bus,
            frames,
            NonNull::from(&mut list),
        )
    };
    if status != 0 {
        return status;
    }
    let samples = &mic.buffer[..frames_len];
    // Taken only while a capture attaches or detaches; this audio is simply skipped.
    let Some(mut sink) = mic.shared.sink.try_lock() else {
        return 0;
    };
    if let Some(sink) = sink.as_mut() {
        match mic.resampler.as_mut() {
            Some(resampler) => sink(&resampler.process(samples)),
            None => sink(samples),
        }
    }
    0
}

/// Rebuilds the unit on its next use when the default devices change, since it stays bound to the
/// devices it started on. Microphones reopen right away through their supervisors.
fn listen_for_route_changes() {
    for selector in [
        kAudioHardwarePropertyDefaultInputDevice,
        kAudioHardwarePropertyDefaultOutputDevice,
    ] {
        let address = AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        // SAFETY: `address` is valid for the call, the listener is a plain function and no client
        // data is passed.
        let status = unsafe {
            AudioObjectAddPropertyListener(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&address),
                Some(on_default_device_changed),
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            log::warn!("Echo-cancelling audio: couldn't watch for device changes ({status})");
        }
    }
}

unsafe extern "C-unwind" fn on_default_device_changed(
    _object: AudioObjectID,
    _count: u32,
    _addresses: NonNull<AudioObjectPropertyAddress>,
    _client_data: *mut c_void,
) -> i32 {
    // Runs on a Core Audio notification thread, so the engine lock is left alone.
    NEEDS_RESTART.store(true, Ordering::Relaxed);
    reopen_captures();
    0
}

/// Microphone levels measured by [`probe_duplex`]: the amplitude of the tone's frequency.
#[doc(hidden)]
#[derive(Debug)]
pub struct ProbeLevels {
    pub echo_cancelling: bool,
    pub silence: f32,
    pub during_tone: f32,
}

/// Starts the unit through the real lease path, records the microphone in silence and then while
/// `tone` (at the unit's speaker rate, of frequency `tone_hz`) plays, with the canceller bypassed
/// if `bypass`. For hardware probes only.
#[doc(hidden)]
pub fn probe_duplex(
    tone: &[f32],
    tone_hz: f32,
    seconds: f32,
    bypass: bool,
) -> anyhow::Result<ProbeLevels> {
    PROBE_BYPASS.store(bypass, Ordering::Relaxed);
    let recorded = Arc::new(Mutex::new(Vec::<f32>::new()));
    let sink_recorded = recorded.clone();
    let capture = acquire_capture(Box::new(move |samples| {
        sink_recorded.lock().extend_from_slice(samples)
    }))?;
    let output = acquire_output()?;
    let echo_cancelling = super::is_echo_cancelling();
    std::thread::sleep(std::time::Duration::from_secs_f32(seconds));
    let silence = std::mem::take(&mut *recorded.lock());
    output.play(tone, Box::new(|| {}));
    std::thread::sleep(std::time::Duration::from_secs_f32(seconds));
    let during = std::mem::take(&mut *recorded.lock());
    drop(output);
    drop(capture);
    PROBE_BYPASS.store(false, Ordering::Relaxed);
    // Skip the canceller's initial adaptation.
    let skip = (CAPTURE_RATE as usize / 2).min(during.len());
    Ok(ProbeLevels {
        echo_cancelling,
        silence: tone_amplitude(&silence, tone_hz),
        during_tone: tone_amplitude(&during[skip..], tone_hz),
    })
}

/// Amplitude of the `hz` component of `samples` (Goertzel).
fn tone_amplitude(samples: &[f32], hz: f32) -> f32 {
    let coefficient = 2.0 * (2.0 * std::f32::consts::PI * hz / CAPTURE_RATE as f32).cos();
    let (mut previous, mut before) = (0.0f32, 0.0f32);
    for &sample in samples {
        let current = sample + coefficient * previous - before;
        before = previous;
        previous = current;
    }
    let power = previous * previous + before * before - coefficient * previous * before;
    2.0 * power.max(0.0).sqrt() / samples.len().max(1) as f32
}
