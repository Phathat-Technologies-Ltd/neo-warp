//! The system pipeline on macOS: a private global process tap (everything any process plays,
//! including our own speech) and the default microphone in one private aggregate device, so both
//! arrive in the same I/O callback with drift compensation. The callback runs AEC3 on each 10 ms
//! frame (far end first, then the microphone) and queues the cleaned, resampled audio in a
//! lock-free ring; a worker thread hands it to the capture sink and watches the tap's health.
//!
//! The I/O callback never locks, allocates or makes Core Audio property calls: those can block
//! against the HAL and stall the device (see the voice-processing unit's microphone callback).

use std::ffi::{CStr, c_void};
use std::mem::size_of;
use std::ptr::{NonNull, null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::time::Duration;

use instant::Instant;

use anyhow::{Context as _, bail};
use objc2::AnyThread as _;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectAddPropertyListener, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectSetPropertyData, CATapDescription,
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceMainSubDeviceKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyProcessObjectList,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioProcessPropertyIsRunningOutput, kAudioSubDeviceUIDKey, kAudioSubTapUIDKey,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioTimeStamp};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{
    NSArray, NSDictionary, NSNumber, NSOperatingSystemVersion, NSProcessInfo, NSString,
};
use parking_lot::Mutex;
use rubato::{FftFixedInOut, Resampler as _};
use sonora::config::{EchoCanceller, NoiseSuppression};
use sonora::{AudioProcessing, Config, StreamConfig};

use super::{TapHealth, TapHealthDetector};
use crate::voice_processing::{CAPTURE_RATE, CaptureSink, SampleRing};

/// Rate requested for the aggregate device; AEC3 also accepts 16 and 32 kHz.
const PREFERRED_RATE: f64 = 48_000.0;
/// Cleaned audio the ring holds before the worker drains it: about 4 s.
const OUTPUT_CAPACITY: usize = 1 << 16;
const DRAIN_INTERVAL: Duration = Duration::from_millis(10);
const HEALTH_INTERVAL: Duration = Duration::from_millis(500);
/// How long a started pipeline may take to deliver its first audio.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
/// A running pipeline that delivers nothing for this long is rebuilt on its next use.
const STALL_AFTER: Duration = Duration::from_secs(1);
/// How long the pipeline stays warm with no microphone attached.
const IDLE_LINGER: Duration = Duration::from_secs(10 * 60);

/// State shared by the I/O callback, the worker thread and leases.
struct Shared {
    output: SampleRing,
    sink: Mutex<Option<CaptureSink>>,
    /// Set by the I/O callback when a frame had tap audio; cleared by the health check.
    tap_had_sound: AtomicBool,
    /// The I/O proc's input layout, `buffers << 16 | mic channels << 8 | tap channels`, for
    /// diagnosing a silent tap.
    input_layout: AtomicU32,
    frames_processed: AtomicU64,
    stop: AtomicBool,
    blocked: AtomicBool,
    /// Never started delivering audio, or stopped delivering it.
    dead: AtomicBool,
}

/// What the I/O callback owns: 10 ms frame accumulators, the canceller and the resampler.
struct AudioState {
    mic: Box<[f32]>,
    tap: Box<[f32]>,
    filled: usize,
    render_out: Box<[f32]>,
    capture_out: Box<[f32]>,
    apm: AudioProcessing,
    resampler: Option<(FftFixedInOut<f32>, Vec<Vec<f32>>)>,
    shared: Arc<Shared>,
}

impl AudioState {
    fn push(&mut self, mic: f32, tap: f32) {
        self.mic[self.filled] = mic;
        self.tap[self.filled] = tap;
        self.filled += 1;
        if self.filled == self.mic.len() {
            self.filled = 0;
            self.process_frame();
        }
    }

    fn process_frame(&mut self) {
        if self.tap.iter().any(|&sample| sample != 0.0) {
            self.shared.tap_had_sound.store(true, Ordering::Relaxed);
        }
        let _ = self
            .apm
            .process_render_f32(&[&self.tap[..]], &mut [&mut self.render_out[..]]);
        if self
            .apm
            .process_capture_f32(&[&self.mic[..]], &mut [&mut self.capture_out[..]])
            .is_err()
        {
            return;
        }
        match &mut self.resampler {
            Some((resampler, out)) => {
                if let Ok((_, produced)) =
                    resampler.process_into_buffer(&[&self.capture_out[..]], out, None)
                {
                    self.shared.output.push(&out[0][..produced]);
                }
            }
            None => {
                self.shared.output.push(&self.capture_out);
            }
        }
        self.shared.frames_processed.fetch_add(1, Ordering::Relaxed);
    }
}

/// Core Audio objects, released in reverse order of creation.
#[derive(Default)]
struct Devices {
    tap: Option<AudioObjectID>,
    aggregate: Option<AudioObjectID>,
    io_proc: AudioDeviceIOProcID,
    started: bool,
}

impl Drop for Devices {
    fn drop(&mut self) {
        // SAFETY: each object was created by this struct and is released once. Stopping the
        // device off the I/O thread waits for the callback to return.
        unsafe {
            if let Some(aggregate) = self.aggregate {
                if self.started {
                    AudioDeviceStop(aggregate, self.io_proc);
                }
                if self.io_proc.is_some() {
                    AudioDeviceDestroyIOProcID(aggregate, self.io_proc);
                }
                AudioHardwareDestroyAggregateDevice(aggregate);
            }
            if let Some(tap) = self.tap {
                AudioHardwareDestroyProcessTap(tap);
            }
        }
    }
}

struct Engine {
    /// Declared before `audio` so the device stops before the state its callback uses is freed.
    _devices: Devices,
    _audio: Box<AudioState>,
    shared: Arc<Shared>,
    captures: usize,
    idle_since: Option<Instant>,
}

// SAFETY: the audio state is only touched by the I/O callback, which stops before the engine is
// dropped; the rest is plain data and thread-safe shared state.
unsafe impl Send for Engine {}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        *self.shared.sink.lock() = None;
        log::info!("System echo cancellation: pipeline stopped");
        crate::voice_processing::notify_backend_changed();
    }
}

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);
/// Set when the default devices or settings change; the pipeline is rebuilt on its next use.
static NEEDS_REBUILD: AtomicBool = AtomicBool::new(false);
static LISTEN_FOR_DEVICE_CHANGES: Once = Once::new();

pub(super) fn os_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
            majorVersion: 14,
            minorVersion: 2,
            patchVersion: 0,
        })
    })
}

fn is_live(shared: &Shared) -> bool {
    !shared.blocked.load(Ordering::Relaxed) && !shared.dead.load(Ordering::Relaxed)
}

pub(super) fn is_running() -> bool {
    ENGINE
        .lock()
        .as_ref()
        .is_some_and(|engine| is_live(&engine.shared))
}

pub(super) fn is_cancelling() -> bool {
    ENGINE
        .lock()
        .as_ref()
        .is_some_and(|engine| engine.captures > 0 && is_live(&engine.shared))
}

pub(super) fn shutdown() {
    let engine = ENGINE.lock().take();
    drop(engine);
}

pub(super) fn on_noise_suppression_changed() {
    NEEDS_REBUILD.store(true, Ordering::Relaxed);
}

/// A microphone attached to the pipeline; dropping it detaches the sink.
pub(crate) struct SystemCaptureLease {
    shared: Arc<Shared>,
}

impl Drop for SystemCaptureLease {
    fn drop(&mut self) {
        *self.shared.sink.lock() = None;
        let mut guard = ENGINE.lock();
        if let Some(engine) = guard.as_mut()
            && Arc::ptr_eq(&engine.shared, &self.shared)
        {
            engine.captures = engine.captures.saturating_sub(1);
            if engine.captures == 0 {
                engine.idle_since = Some(Instant::now());
            }
        }
    }
}

pub(super) fn acquire_capture(sink: CaptureSink) -> anyhow::Result<SystemCaptureLease> {
    LISTEN_FOR_DEVICE_CHANGES.call_once(listen_for_device_changes);
    let mut guard = ENGINE.lock();
    let rebuild = NEEDS_REBUILD.swap(false, Ordering::Relaxed);
    if let Some(engine) = guard.as_ref()
        && (rebuild || !is_live(&engine.shared))
    {
        log::info!("System echo cancellation: rebuilding the pipeline");
        *guard = None;
    }
    if guard.is_none() {
        *guard = Some(start_engine()?);
    }
    let engine = guard.as_mut().context("the pipeline isn't running")?;
    *engine.shared.sink.lock() = Some(sink);
    engine.captures += 1;
    engine.idle_since = None;
    Ok(SystemCaptureLease {
        shared: engine.shared.clone(),
    })
}

fn check(status: i32, what: &str) -> anyhow::Result<()> {
    if status != 0 {
        bail!(
            "{what} failed: {status} ({})",
            String::from_utf8_lossy(&status.to_be_bytes())
        );
    }
    Ok(())
}

fn address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Reads a fixed-size global property.
///
/// # Safety
/// `T` must match the property's type.
unsafe fn get<T: Copy>(object: AudioObjectID, selector: u32, init: T) -> anyhow::Result<T> {
    let mut value = init;
    let mut size = size_of::<T>() as u32;
    let address = address(selector);
    // SAFETY: `value` has room for `size` bytes of the property's type.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    check(status, "AudioObjectGetPropertyData")?;
    Ok(value)
}

fn device_uid(device: AudioObjectID) -> anyhow::Result<Retained<NSString>> {
    // SAFETY: the UID property is a CFStringRef, returned +1 and toll-free bridged to NSString.
    unsafe {
        let uid: *mut NSString = get(device, kAudioDevicePropertyDeviceUID, null_mut())?;
        Retained::from_raw(uid).context("the device has no UID")
    }
}

fn key(name: &CStr) -> Retained<NSString> {
    NSString::from_str(&name.to_string_lossy())
}

fn string_object(value: Retained<NSString>) -> Retained<AnyObject> {
    Retained::into_super(Retained::into_super(value))
}

fn bool_object(value: bool) -> Retained<AnyObject> {
    Retained::into_super(Retained::into_super(Retained::into_super(
        NSNumber::new_bool(value),
    )))
}

fn dictionary(
    keys: &[Retained<NSString>],
    objects: &[Retained<AnyObject>],
) -> Retained<NSDictionary<NSString, AnyObject>> {
    let keys: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
    NSDictionary::from_retained_objects(&keys, objects)
}

fn dictionary_list(dictionary: Retained<NSDictionary<NSString, AnyObject>>) -> Retained<AnyObject> {
    Retained::into_super(Retained::into_super(NSArray::from_retained_slice(&[
        dictionary,
    ])))
}

fn build_processor(rate: u32) -> AudioProcessing {
    let noise_suppression = super::noise_suppression().then(NoiseSuppression::default);
    AudioProcessing::builder()
        .config(Config {
            echo_canceller: Some(EchoCanceller::default()),
            noise_suppression,
            ..Default::default()
        })
        .capture_config(StreamConfig::new(rate, 1))
        .render_config(StreamConfig::new(rate, 1))
        .build()
}

fn start_engine() -> anyhow::Result<Engine> {
    let started_at = Instant::now();
    let mut devices = Devices::default();
    // SAFETY: plain Core Audio HAL calls with valid arguments; the created objects are owned by
    // `devices`, which releases them on failure.
    unsafe {
        let description = CATapDescription::initStereoGlobalTapButExcludeProcesses(
            CATapDescription::alloc(),
            &NSArray::new(),
        );
        description.setPrivate(true);
        description.setName(&NSString::from_str("neo-warp echo cancellation"));
        let mut tap = 0;
        check(
            AudioHardwareCreateProcessTap(Some(&description), &mut tap),
            "AudioHardwareCreateProcessTap",
        )?;
        devices.tap = Some(tap);
        let tap_uid = description.UUID().UUIDString();

        let input_device: AudioObjectID = get(
            kAudioObjectSystemObject as AudioObjectID,
            kAudioHardwarePropertyDefaultInputDevice,
            0,
        )?;
        let input_uid = device_uid(input_device)?;
        let sub_device = dictionary(
            &[key(kAudioSubDeviceUIDKey)],
            &[string_object(input_uid.clone())],
        );
        let sub_tap = dictionary(
            &[key(kAudioSubTapUIDKey), NSString::from_str("drift")],
            &[string_object(tap_uid.clone()), bool_object(true)],
        );
        let aggregate_description = dictionary(
            &[
                key(kAudioAggregateDeviceUIDKey),
                key(kAudioAggregateDeviceNameKey),
                key(kAudioAggregateDeviceMainSubDeviceKey),
                key(kAudioAggregateDeviceIsPrivateKey),
                key(kAudioAggregateDeviceTapAutoStartKey),
                key(kAudioAggregateDeviceSubDeviceListKey),
                key(kAudioAggregateDeviceTapListKey),
            ],
            &[
                string_object(NSString::from_str(&format!(
                    "dev.warp.system-echo-cancellation.{tap_uid}"
                ))),
                string_object(NSString::from_str("neo-warp echo cancellation")),
                string_object(input_uid),
                bool_object(true),
                bool_object(true),
                dictionary_list(sub_device),
                dictionary_list(sub_tap),
            ],
        );
        let cf_description = &*(Retained::as_ptr(&aggregate_description) as *const CFDictionary);
        let mut aggregate = 0;
        check(
            AudioHardwareCreateAggregateDevice(cf_description, NonNull::from(&mut aggregate)),
            "AudioHardwareCreateAggregateDevice",
        )?;
        devices.aggregate = Some(aggregate);

        let rate_address = address(kAudioDevicePropertyNominalSampleRate);
        let preferred = PREFERRED_RATE;
        let status = AudioObjectSetPropertyData(
            aggregate,
            NonNull::from(&rate_address),
            0,
            null(),
            size_of::<f64>() as u32,
            NonNull::from(&preferred).cast(),
        );
        if status != 0 {
            log::debug!("System echo cancellation: couldn't set the rate to 48 kHz ({status})");
        }
        let rate: f64 = get(aggregate, kAudioDevicePropertyNominalSampleRate, 0.0)?;
        let rate_hz = rate.round() as u32;
        if ![16_000, 32_000, 48_000].contains(&rate_hz) {
            bail!("the aggregate device runs at {rate} Hz, which AEC3 doesn't support");
        }
        let frame = rate_hz as usize / 100;
        let resampler = if rate_hz == CAPTURE_RATE as u32 {
            None
        } else {
            let resampler =
                FftFixedInOut::<f32>::new(rate_hz as usize, CAPTURE_RATE as usize, frame, 1)
                    .context("failed to create the resampler")?;
            let out = vec![vec![0.0; resampler.output_frames_max()]];
            Some((resampler, out))
        };

        let shared = Arc::new(Shared {
            output: SampleRing::new(OUTPUT_CAPACITY),
            sink: Mutex::new(None),
            tap_had_sound: AtomicBool::new(false),
            input_layout: AtomicU32::new(0),
            frames_processed: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            blocked: AtomicBool::new(false),
            dead: AtomicBool::new(false),
        });
        let mut audio = Box::new(AudioState {
            mic: vec![0.0; frame].into(),
            tap: vec![0.0; frame].into(),
            filled: 0,
            render_out: vec![0.0; frame].into(),
            capture_out: vec![0.0; frame].into(),
            apm: build_processor(rate_hz),
            resampler,
            shared: shared.clone(),
        });

        check(
            AudioDeviceCreateIOProcID(
                aggregate,
                Some(io_proc),
                (&raw mut *audio).cast(),
                NonNull::from(&mut devices.io_proc),
            ),
            "AudioDeviceCreateIOProcID",
        )?;
        check(
            AudioDeviceStart(aggregate, devices.io_proc),
            "AudioDeviceStart",
        )?;
        devices.started = true;

        let worker_shared = shared.clone();
        std::thread::Builder::new()
            .name("system-echo-cancellation".to_owned())
            .spawn(move || run_worker(worker_shared, started_at))
            .context("failed to start the echo cancellation thread")?;
        log::info!(
            "System echo cancellation: pipeline started at {rate_hz} Hz in {} ms (noise \
             suppression {})",
            started_at.elapsed().as_millis(),
            if super::noise_suppression() {
                "on"
            } else {
                "off"
            }
        );
        Ok(Engine {
            _devices: devices,
            _audio: audio,
            shared,
            captures: 0,
            idle_since: Some(Instant::now()),
        })
    }
}

unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let Some(mut state) = NonNull::new(client.cast::<AudioState>()) else {
        return 0;
    };
    // SAFETY: `client` is the engine's `AudioState`, which outlives the I/O proc; the I/O thread
    // is its only user.
    let state = unsafe { state.as_mut() };
    // SAFETY: Core Audio passes a valid buffer list for the duration of the call.
    let list = unsafe { input.as_ref() };
    let buffers =
        unsafe { std::slice::from_raw_parts(list.mBuffers.as_ptr(), list.mNumberBuffers as usize) };
    // The microphone's streams come first and the tap's last.
    let (Some(mic), Some(tap)) = (buffers.first(), buffers.last()) else {
        return 0;
    };
    if buffers.len() < 2 {
        return 0;
    }
    let samples = |buffer: &AudioBuffer| {
        if buffer.mData.is_null() {
            return &[][..];
        }
        // SAFETY: the buffer holds `mDataByteSize` bytes of `f32` samples.
        unsafe {
            std::slice::from_raw_parts(
                buffer.mData as *const f32,
                buffer.mDataByteSize as usize / size_of::<f32>(),
            )
        }
    };
    let mic_channels = mic.mNumberChannels.max(1) as usize;
    let tap_channels = tap.mNumberChannels.max(1) as usize;
    state.shared.input_layout.store(
        (buffers.len() as u32) << 16
            | (mic_channels as u32 & 0xff) << 8
            | tap_channels as u32 & 0xff,
        Ordering::Relaxed,
    );
    let mic_frames = samples(mic).chunks_exact(mic_channels);
    let tap_frames = samples(tap).chunks_exact(tap_channels);
    for (mic_frame, tap_frame) in mic_frames.zip(tap_frames) {
        let far = tap_frame.iter().sum::<f32>() / tap_channels as f32;
        state.push(mic_frame[0], far);
    }
    0
}

/// Hands cleaned audio to the sink, watches the tap's health, and stops an idle pipeline.
fn run_worker(shared: Arc<Shared>, started_at: Instant) {
    let mut buffer = vec![0.0f32; 4096];
    let mut detector = TapHealthDetector::default();
    let mut last_check = Instant::now();
    let mut last_frames = 0;
    let mut last_progress = Instant::now();
    let mut first_audio_logged = false;
    while !shared.stop.load(Ordering::Relaxed) {
        std::thread::sleep(DRAIN_INTERVAL);
        let frames = shared.frames_processed.load(Ordering::Relaxed);
        loop {
            let count = shared.output.available().min(buffer.len());
            if count == 0 {
                break;
            }
            shared.output.pop_into(&mut buffer[..count]);
            if let Some(sink) = shared.sink.lock().as_mut() {
                sink(&buffer[..count]);
            }
        }
        if frames == 0 {
            if started_at.elapsed() < STARTUP_TIMEOUT {
                // Keeps the microphone supervisor from treating the startup delay as a stall.
                if let Some(sink) = shared.sink.lock().as_mut() {
                    sink(&[]);
                }
            } else if !shared.dead.swap(true, Ordering::Relaxed) {
                log::warn!(
                    "System echo cancellation: no audio {} s after starting; falling back",
                    STARTUP_TIMEOUT.as_secs()
                );
                super::on_never_started();
            }
        } else {
            if !first_audio_logged {
                first_audio_logged = true;
                let layout = shared.input_layout.load(Ordering::Relaxed);
                log::info!(
                    "System echo cancellation: first audio {} ms after starting ({} input \
                     buffers, microphone {} channels, tap {} channels)",
                    started_at.elapsed().as_millis(),
                    layout >> 16,
                    (layout >> 8) & 0xff,
                    layout & 0xff
                );
            }
            if frames != last_frames {
                last_frames = frames;
                last_progress = Instant::now();
            } else if last_progress.elapsed() >= STALL_AFTER
                && !shared.dead.swap(true, Ordering::Relaxed)
            {
                log::info!("System echo cancellation: the pipeline stopped delivering audio");
                crate::voice_processing::reopen_captures();
            }
        }
        if last_check.elapsed() < HEALTH_INTERVAL {
            continue;
        }
        let elapsed = last_check.elapsed();
        last_check = Instant::now();
        if frames > 0 {
            let had_sound = shared.tap_had_sound.swap(false, Ordering::Relaxed);
            let playing = had_sound || something_is_playing();
            match detector.observe(had_sound, playing, elapsed) {
                Some(TapHealth::Healthy) => {
                    log::info!("System echo cancellation: the tap is delivering system audio");
                }
                Some(TapHealth::Blocked) => {
                    shared.blocked.store(true, Ordering::Relaxed);
                    super::on_blocked();
                }
                Some(TapHealth::Unknown) | None => {}
            }
        }
        reap_if_idle(&shared);
    }
}

/// Drops the pipeline if it has no microphone and is broken or has been idle for a while.
fn reap_if_idle(shared: &Arc<Shared>) {
    let mut guard = ENGINE.lock();
    let Some(engine) = guard.as_ref() else {
        return;
    };
    if !Arc::ptr_eq(&engine.shared, shared) || engine.captures > 0 {
        return;
    }
    let expired = engine
        .idle_since
        .is_some_and(|since| since.elapsed() >= IDLE_LINGER);
    if expired || !is_live(shared) {
        let engine = guard.take();
        drop(guard);
        drop(engine);
    }
}

/// Whether any process (other than the aggregate's input) is sending audio to an output device.
fn something_is_playing() -> bool {
    let system = kAudioObjectSystemObject as AudioObjectID;
    let list_address = address(kAudioHardwarePropertyProcessObjectList);
    let mut size = 0u32;
    // SAFETY: valid address and out-pointer.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            system,
            NonNull::from(&list_address),
            0,
            null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 {
        return false;
    }
    let mut processes = vec![0 as AudioObjectID; size as usize / size_of::<AudioObjectID>()];
    // SAFETY: `processes` has room for `size` bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            system,
            NonNull::from(&list_address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::new(processes.as_mut_ptr().cast()).expect("vector pointer is never null"),
        )
    };
    if status != 0 {
        return false;
    }
    processes.truncate(size as usize / size_of::<AudioObjectID>());
    processes.into_iter().any(|process| {
        // SAFETY: the property is a UInt32.
        unsafe { get::<u32>(process, kAudioProcessPropertyIsRunningOutput, 0) }
            .is_ok_and(|running| running != 0)
    })
}

/// Rebuilds the pipeline on its next use when the default devices change, since the aggregate is
/// bound to the microphone it was created with.
fn listen_for_device_changes() {
    for selector in [
        kAudioHardwarePropertyDefaultInputDevice,
        kAudioHardwarePropertyDefaultOutputDevice,
    ] {
        let address = address(selector);
        // SAFETY: `address` is valid for the call, the listener is a plain function and no client
        // data is passed.
        let status = unsafe {
            AudioObjectAddPropertyListener(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&address),
                Some(on_default_device_changed),
                null_mut(),
            )
        };
        if status != 0 {
            log::warn!("System echo cancellation: couldn't watch for device changes ({status})");
        }
    }
}

unsafe extern "C-unwind" fn on_default_device_changed(
    _object: AudioObjectID,
    _count: u32,
    _addresses: NonNull<AudioObjectPropertyAddress>,
    _client_data: *mut c_void,
) -> i32 {
    log::info!("System echo cancellation: default device changed");
    NEEDS_REBUILD.store(true, Ordering::Relaxed);
    super::on_devices_changed();
    0
}
