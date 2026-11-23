//! System-wide acoustic echo cancellation: a Core Audio process tap records everything the Mac
//! plays (including our own speech) and WebRTC AEC3 subtracts it from the microphone, so audio
//! from any app is cancelled. macOS 14.2+ only; elsewhere it is never available and callers use
//! the voice-processing unit or plain cpal.
//!
//! The pipeline is started on first use and kept warm for reuse, since starting it takes seconds.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;

use super::{AudioBackend, CaptureSink};

#[cfg(all(target_os = "macos", feature = "voice_processing"))]
mod macos;
#[cfg(all(target_os = "macos", feature = "voice_processing"))]
use macos as imp;

#[cfg(not(all(target_os = "macos", feature = "voice_processing")))]
mod unsupported;
#[cfg(not(all(target_os = "macos", feature = "voice_processing")))]
use unsupported as imp;

pub(crate) use imp::SystemCaptureLease;

/// How long the tap may stay silent while another process is playing audio before the pipeline is
/// considered blocked: macOS feeds the tap zeros instead of failing when system audio recording
/// isn't allowed.
const SILENT_TAP_LIMIT: Duration = Duration::from_secs(3);
/// Consecutive failed starts after which the pipeline isn't tried again until it is re-enabled or
/// the default devices change.
const MAX_START_FAILURES: u32 = 2;

/// Whether the tap is known to deliver the audio being played.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapHealth {
    /// Nothing conclusive has been observed yet.
    Unknown,
    /// The tap has delivered sound.
    Healthy,
    /// The tap stayed silent while something was playing.
    Blocked,
}

/// Decides from periodic observations whether the tap works.
#[derive(Debug, Clone)]
pub struct TapHealthDetector {
    health: TapHealth,
    silent_while_playing: Duration,
}

impl Default for TapHealthDetector {
    fn default() -> Self {
        Self {
            health: TapHealth::Unknown,
            silent_while_playing: Duration::ZERO,
        }
    }
}

impl TapHealthDetector {
    pub fn health(&self) -> TapHealth {
        self.health
    }

    /// Records an interval of `elapsed` during which the tap did or didn't carry sound and some
    /// process was or wasn't playing audio. Returns the new health when it changed. A verdict is
    /// final for the life of the pipeline.
    pub fn observe(
        &mut self,
        tap_had_sound: bool,
        something_playing: bool,
        elapsed: Duration,
    ) -> Option<TapHealth> {
        if self.health != TapHealth::Unknown {
            return None;
        }
        if tap_had_sound {
            self.health = TapHealth::Healthy;
            return Some(self.health);
        }
        if something_playing {
            self.silent_while_playing += elapsed;
            if self.silent_while_playing >= SILENT_TAP_LIMIT {
                self.health = TapHealth::Blocked;
                return Some(self.health);
            }
        }
        None
    }
}

/// Whether the system pipeline may be tried for a new microphone.
#[derive(Debug, Clone, Default)]
pub struct SystemAecSelector {
    wanted: bool,
    blocked: bool,
    failures: u32,
}

impl SystemAecSelector {
    /// Returns whether the preference changed. Turning it back on forgets earlier failures and a
    /// blocked tap, so the user can retry after granting access.
    pub fn set_wanted(&mut self, wanted: bool) -> bool {
        let changed = self.wanted != wanted;
        self.wanted = wanted;
        if changed && wanted {
            self.failures = 0;
            self.blocked = false;
        }
        changed
    }

    pub fn should_try(&self, os_supported: bool) -> bool {
        self.wanted && os_supported && !self.blocked && self.failures < MAX_START_FAILURES
    }

    pub fn on_started(&mut self) {
        self.failures = 0;
    }

    pub fn on_start_failed(&mut self) {
        self.failures += 1;
    }

    /// The pipeline started but never delivered audio; don't retry it until the devices change.
    pub fn on_never_started(&mut self) {
        self.failures = MAX_START_FAILURES;
    }

    pub fn on_blocked(&mut self) {
        self.blocked = true;
    }

    /// The default devices changed, so a pipeline that failed to start may work now.
    pub fn on_devices_changed(&mut self) {
        self.failures = 0;
    }
}

/// The backend for a new speaker. While the system pipeline runs, speech plays through plain cpal:
/// the tap already hears it, and the voice-processing unit would fight over the microphone.
pub fn output_backend(system_aec_running: bool, voice_processing: AudioBackend) -> AudioBackend {
    if system_aec_running {
        AudioBackend::Cpal
    } else {
        voice_processing
    }
}

static SELECTOR: Mutex<SystemAecSelector> = Mutex::new(SystemAecSelector {
    wanted: false,
    blocked: false,
    failures: 0,
});
static NOISE_SUPPRESSION: AtomicBool = AtomicBool::new(false);
static NOTICE_SENT: AtomicBool = AtomicBool::new(false);
static NOTICES: Mutex<Option<async_channel::Sender<()>>> = Mutex::new(None);

/// Sets whether microphones use system-wide echo cancellation when it works, and whether its
/// noise suppression runs (it should be off when another denoiser runs). Open microphones move to
/// the new backend.
pub fn set_system_echo_cancellation(wanted: bool, noise_suppression: bool) {
    let suppression_changed =
        NOISE_SUPPRESSION.swap(noise_suppression, Ordering::Relaxed) != noise_suppression;
    let wanted_changed = SELECTOR.lock().set_wanted(wanted);
    if wanted_changed {
        log::info!(
            "System echo cancellation {}",
            if wanted { "wanted" } else { "off" }
        );
        if wanted {
            NOTICE_SENT.store(false, Ordering::Relaxed);
        } else {
            imp::shutdown();
        }
    }
    if suppression_changed {
        imp::on_noise_suppression_changed();
    }
    if wanted_changed || suppression_changed {
        super::reopen_captures();
    }
}

/// Receives one message the first time the tap turns out to be blocked, so the app can ask the
/// user to allow system audio recording.
pub fn blocked_notices() -> async_channel::Receiver<()> {
    let (tx, rx) = async_channel::bounded(1);
    if NOTICE_SENT.load(Ordering::Relaxed) {
        let _ = tx.try_send(());
    }
    *NOTICES.lock() = Some(tx);
    rx
}

/// Whether the system pipeline is running and not known to be blocked.
pub fn is_running() -> bool {
    imp::is_running()
}

/// Whether a microphone currently gets system-wide echo cancellation.
pub(crate) fn is_cancelling() -> bool {
    imp::is_cancelling()
}

pub(crate) fn noise_suppression() -> bool {
    NOISE_SUPPRESSION.load(Ordering::Relaxed)
}

pub(crate) fn should_try() -> bool {
    SELECTOR.lock().should_try(imp::os_supported())
}

/// Attaches `sink` to the pipeline, starting it if needed. The sink receives mono audio at
/// [`super::CAPTURE_RATE`] from a non-real-time thread.
pub(crate) fn acquire_capture(sink: CaptureSink) -> anyhow::Result<SystemCaptureLease> {
    match imp::acquire_capture(sink) {
        Ok(lease) => {
            SELECTOR.lock().on_started();
            super::notify_backend_changed();
            Ok(lease)
        }
        Err(error) => {
            SELECTOR.lock().on_start_failed();
            Err(error)
        }
    }
}

#[cfg_attr(
    not(all(target_os = "macos", feature = "voice_processing")),
    allow(dead_code)
)]
fn on_blocked() {
    SELECTOR.lock().on_blocked();
    log::info!(
        "System echo cancellation: the system audio tap is silent while audio plays; falling back"
    );
    if !NOTICE_SENT.swap(true, Ordering::Relaxed)
        && let Some(tx) = NOTICES.lock().as_ref()
    {
        let _ = tx.try_send(());
    }
    super::reopen_captures();
}

#[cfg_attr(
    not(all(target_os = "macos", feature = "voice_processing")),
    allow(dead_code)
)]
fn on_never_started() {
    SELECTOR.lock().on_never_started();
    super::reopen_captures();
}

#[cfg_attr(
    not(all(target_os = "macos", feature = "voice_processing")),
    allow(dead_code)
)]
fn on_devices_changed() {
    SELECTOR.lock().on_devices_changed();
    super::reopen_captures();
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
