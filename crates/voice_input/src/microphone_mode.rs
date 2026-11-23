//! The macOS microphone mode (Control Center > Mic Mode). The user picks the mode in a system
//! picker; apps can only read it and open the picker. It applies only to capture through Apple's
//! voice processing, so the plain cpal path and the system-wide echo cancellation pipeline (a
//! process tap and aggregate device without voice processing) never get Voice Isolation.

use std::sync::Once;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use parking_lot::Mutex;

use crate::voice_processing::{self, AudioBackend};

/// How often the mode is re-read while a microphone is open.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicMode {
    Standard,
    WideSpectrum,
    VoiceIsolation,
}

impl MicMode {
    pub fn label(self) -> &'static str {
        match self {
            MicMode::Standard => "Standard",
            MicMode::WideSpectrum => "Wide Spectrum",
            MicMode::VoiceIsolation => "Voice Isolation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicModeState {
    /// The mode processing this app's microphone audio now; it falls back to
    /// [`MicMode::Standard`] when the active route doesn't support the preferred mode.
    pub active: MicMode,
    /// The mode the user picked in Control Center.
    pub preferred: MicMode,
}

/// The current microphone mode, or `None` where the OS has no microphone modes.
pub fn microphone_mode() -> Option<MicModeState> {
    platform::microphone_mode()
}

/// Opens the system Mic Mode picker. Returns whether this platform has one.
pub fn show_microphone_mode_picker() -> bool {
    platform::show_picker()
}

/// What the Settings row says about the microphone mode; `capturing` is whether a microphone is
/// open on a path that honours the mode.
pub fn status_label(
    state: Option<MicModeState>,
    system_wide_cancelling: bool,
    capturing: bool,
) -> String {
    match state {
        None => "Unavailable on this computer".to_owned(),
        Some(_) if system_wide_cancelling => {
            "Not available while cancelling sound from other apps".to_owned()
        }
        Some(state) if capturing && state.active != state.preferred => {
            format!(
                "{} (not active on this microphone)",
                state.preferred.label()
            )
        }
        Some(state) => state.preferred.label().to_owned(),
    }
}

/// Whether to suggest turning on Voice Isolation: voice capture is live on a path that honours
/// the mode, the user hasn't picked it, and hasn't dismissed the hint.
pub fn should_suggest_voice_isolation(
    state: Option<MicModeState>,
    backend: Option<AudioBackend>,
    dismissed: bool,
) -> bool {
    !dismissed
        && backend == Some(AudioBackend::VoiceProcessing)
        && state.is_some_and(|state| state.preferred != MicMode::VoiceIsolation)
}

const NO_BACKEND: u8 = u8::MAX;
static CAPTURE_BACKEND: AtomicU8 = AtomicU8::new(NO_BACKEND);

fn encode(backend: AudioBackend) -> u8 {
    match backend {
        AudioBackend::Cpal => 0,
        AudioBackend::VoiceProcessing => 1,
        AudioBackend::SystemAec => 2,
    }
}

pub(crate) fn set_capture_backend(backend: AudioBackend) {
    CAPTURE_BACKEND.store(encode(backend), Ordering::Relaxed);
}

/// The backend of the open hands-free microphone, if any.
pub fn capture_backend() -> Option<AudioBackend> {
    if !voice_processing::has_open_captures() {
        return None;
    }
    match CAPTURE_BACKEND.load(Ordering::Relaxed) {
        0 => Some(AudioBackend::Cpal),
        1 => Some(AudioBackend::VoiceProcessing),
        2 => Some(AudioBackend::SystemAec),
        _ => None,
    }
}

static LISTENERS: Mutex<Vec<async_channel::Sender<()>>> = Mutex::new(Vec::new());
static WATCHER: Once = Once::new();

/// Receives a message when the microphone mode changes while a microphone is open. Bursts are
/// coalesced.
pub fn changes() -> async_channel::Receiver<()> {
    let (tx, rx) = async_channel::bounded(1);
    LISTENERS.lock().push(tx);
    WATCHER.call_once(|| {
        if microphone_mode().is_none() {
            return;
        }
        // Polled rather than key-value observed: the thread needs no run loop and the mode only
        // changes while the user is in the Control Center picker.
        let spawned = std::thread::Builder::new()
            .name("mic-mode-watcher".to_owned())
            .spawn(|| {
                let mut last = microphone_mode();
                loop {
                    std::thread::sleep(POLL_INTERVAL);
                    if !voice_processing::has_open_captures() {
                        continue;
                    }
                    let current = microphone_mode();
                    if current != last {
                        if let Some(state) = current {
                            log::info!(
                                "Microphone mode changed: active {}, preferred {}",
                                state.active.label(),
                                state.preferred.label()
                            );
                        }
                        last = current;
                        LISTENERS.lock().retain(|tx| {
                            !matches!(tx.try_send(()), Err(async_channel::TrySendError::Closed(_)))
                        });
                    }
                }
            });
        if let Err(error) = spawned {
            log::warn!("Could not watch the microphone mode: {error}");
        }
    });
    rx
}

#[cfg(all(target_os = "macos", feature = "voice_processing"))]
mod platform {
    use objc2_av_foundation::{
        AVCaptureDevice, AVCaptureMicrophoneMode, AVCaptureSystemUserInterface,
    };
    use objc2_foundation::NSProcessInfo;

    use super::{MicMode, MicModeState};

    fn is_available() -> bool {
        NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(
            objc2_foundation::NSOperatingSystemVersion {
                majorVersion: 12,
                minorVersion: 0,
                patchVersion: 0,
            },
        )
    }

    fn from_av(mode: AVCaptureMicrophoneMode) -> MicMode {
        match mode {
            AVCaptureMicrophoneMode::WideSpectrum => MicMode::WideSpectrum,
            AVCaptureMicrophoneMode::VoiceIsolation => MicMode::VoiceIsolation,
            _ => MicMode::Standard,
        }
    }

    pub(super) fn microphone_mode() -> Option<MicModeState> {
        if !is_available() {
            return None;
        }
        // SAFETY: class properties available since macOS 12, checked above.
        let (active, preferred) = unsafe {
            (
                AVCaptureDevice::activeMicrophoneMode(),
                AVCaptureDevice::preferredMicrophoneMode(),
            )
        };
        Some(MicModeState {
            active: from_av(active),
            preferred: from_av(preferred),
        })
    }

    pub(super) fn show_picker() -> bool {
        if !is_available() {
            return false;
        }
        // SAFETY: available since macOS 12, checked above; non-blocking.
        unsafe {
            AVCaptureDevice::showSystemUserInterface(AVCaptureSystemUserInterface::MicrophoneModes)
        };
        true
    }
}

#[cfg(not(all(target_os = "macos", feature = "voice_processing")))]
mod platform {
    use super::MicModeState;

    pub(super) fn microphone_mode() -> Option<MicModeState> {
        None
    }

    pub(super) fn show_picker() -> bool {
        false
    }
}

#[cfg(test)]
#[path = "microphone_mode_tests.rs"]
mod tests;
