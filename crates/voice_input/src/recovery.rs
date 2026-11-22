//! Keeps a microphone stream alive: glitches are ignored, and a stream that fails or stops
//! delivering audio is rebuilt with backoff until it works again or capture gives up.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long the microphone may deliver no audio before its stream is rebuilt.
const STALL_AFTER: Duration = Duration::from_secs(2);
/// The delay before the first rebuild of a failed stream; each further attempt doubles it.
const FIRST_RETRY_AFTER: Duration = Duration::from_millis(250);
/// Rebuilds attempted in a row before capture gives up.
const MAX_REBUILDS: u32 = 5;

/// Counts audio callbacks so the supervisor can tell a live stream from a stalled one.
pub type Heartbeat = Arc<AtomicU64>;

/// Messages to the supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Fault(StreamFault),
    /// Rebuild the stream now, such as after the preferred audio backend or device changed.
    Reopen,
    Stop,
}

/// How bad a stream error is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFault {
    /// A glitch such as a buffer underrun or overrun; the stream keeps running and the affected
    /// audio is simply lost.
    Transient,
    /// The stream is broken and must be rebuilt.
    Fatal,
}

impl StreamFault {
    pub fn classify(kind: cpal::ErrorKind) -> Self {
        match kind {
            cpal::ErrorKind::Xrun
            | cpal::ErrorKind::RealtimeDenied
            | cpal::ErrorKind::DeviceChanged => StreamFault::Transient,
            cpal::ErrorKind::DeviceBusy
            | cpal::ErrorKind::DeviceNotAvailable
            | cpal::ErrorKind::HostUnavailable
            | cpal::ErrorKind::InvalidInput
            | cpal::ErrorKind::PermissionDenied
            | cpal::ErrorKind::ResourceExhausted
            | cpal::ErrorKind::StreamInvalidated
            | cpal::ErrorKind::UnsupportedConfig
            | cpal::ErrorKind::UnsupportedOperation
            | cpal::ErrorKind::BackendError
            | cpal::ErrorKind::Other => StreamFault::Fatal,
            // `ErrorKind` is non-exhaustive; unknown kinds are treated as broken streams.
            #[allow(unreachable_patterns)]
            _ => StreamFault::Fatal,
        }
    }
}

/// What the supervisor does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryDecision {
    KeepRunning,
    RebuildAfter(Duration),
    GiveUp,
}

/// Decides when a broken or stalled stream is rebuilt and when capture gives up.
#[derive(Debug, Clone)]
pub struct RecoveryPolicy {
    first_retry_after: Duration,
    max_rebuilds: u32,
    /// Rebuilds in a row since the stream last delivered audio.
    rebuilds: u32,
}

impl RecoveryPolicy {
    pub fn new(first_retry_after: Duration, max_rebuilds: u32) -> Self {
        Self {
            first_retry_after,
            max_rebuilds,
            rebuilds: 0,
        }
    }

    pub fn on_fault(&mut self, fault: StreamFault) -> RecoveryDecision {
        match fault {
            StreamFault::Transient => RecoveryDecision::KeepRunning,
            StreamFault::Fatal => self.retry(),
        }
    }

    /// The stream delivered no audio for too long, or reopening it failed.
    pub fn on_failure(&mut self) -> RecoveryDecision {
        self.retry()
    }

    /// The stream delivered audio, so later failures start the backoff over.
    pub fn on_healthy(&mut self) {
        self.rebuilds = 0;
    }

    fn retry(&mut self) -> RecoveryDecision {
        if self.rebuilds >= self.max_rebuilds {
            return RecoveryDecision::GiveUp;
        }
        let delay = self.first_retry_after * 2u32.saturating_pow(self.rebuilds);
        self.rebuilds += 1;
        RecoveryDecision::RebuildAfter(delay)
    }
}

/// Timing of stall detection and rebuilds.
#[derive(Debug, Clone, Copy)]
pub struct RecoveryTiming {
    pub stall_after: Duration,
    pub first_retry_after: Duration,
    pub max_rebuilds: u32,
}

impl Default for RecoveryTiming {
    fn default() -> Self {
        Self {
            stall_after: STALL_AFTER,
            first_retry_after: FIRST_RETRY_AFTER,
            max_rebuilds: MAX_REBUILDS,
        }
    }
}

/// Keeps a stream from `open` alive until [`Signal::Stop`] or until every sender is gone. Blocks
/// the calling thread. `on_started` gets the result of the first open (nothing more happens if it
/// fails); `on_failed` is called if recovery gives up.
pub fn supervise<S>(
    mut open: impl FnMut(Heartbeat) -> anyhow::Result<S>,
    signals: mpsc::Receiver<Signal>,
    timing: RecoveryTiming,
    on_started: impl FnOnce(anyhow::Result<()>),
    on_failed: impl FnOnce(String),
) {
    let mut policy = RecoveryPolicy::new(timing.first_retry_after, timing.max_rebuilds);
    let heartbeat: Heartbeat = Arc::default();
    let mut stream = match open(heartbeat.clone()) {
        Ok(stream) => Some(stream),
        Err(error) => {
            on_started(Err(error));
            return;
        }
    };
    on_started(Ok(()));
    let mut seen_beats = heartbeat.load(Ordering::Relaxed);
    let mut last_audio = Instant::now();
    let mut last_error = "the microphone stopped delivering audio".to_owned();
    loop {
        let mut decision = match signals.recv_timeout(timing.stall_after / 4) {
            Ok(Signal::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Ok(Signal::Fault(fault)) => {
                if fault == StreamFault::Fatal {
                    last_error = "the microphone stream failed".to_owned();
                }
                policy.on_fault(fault)
            }
            Ok(Signal::Reopen) => RecoveryDecision::RebuildAfter(Duration::ZERO),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let beats = heartbeat.load(Ordering::Relaxed);
                if beats != seen_beats {
                    seen_beats = beats;
                    last_audio = Instant::now();
                    policy.on_healthy();
                    RecoveryDecision::KeepRunning
                } else if last_audio.elapsed() >= timing.stall_after {
                    log::warn!("Hands-free microphone delivered no audio; reopening it");
                    last_error = "the microphone stopped delivering audio".to_owned();
                    policy.on_failure()
                } else {
                    RecoveryDecision::KeepRunning
                }
            }
        };
        while let RecoveryDecision::RebuildAfter(delay) = decision {
            stream = None;
            match signals.recv_timeout(delay) {
                Ok(Signal::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                // Faults of the stream just dropped no longer matter.
                Ok(Signal::Fault(_) | Signal::Reopen) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            match open(heartbeat.clone()) {
                Ok(new_stream) => {
                    log::info!("Hands-free microphone reopened");
                    stream = Some(new_stream);
                    last_audio = Instant::now();
                    seen_beats = heartbeat.load(Ordering::Relaxed);
                    decision = RecoveryDecision::KeepRunning;
                }
                Err(error) => {
                    log::warn!("Couldn't reopen the hands-free microphone: {error:#}");
                    last_error = format!("{error:#}");
                    decision = policy.on_failure();
                }
            }
        }
        if decision == RecoveryDecision::GiveUp {
            drop(stream);
            on_failed(format!("Couldn't recover the microphone: {last_error}"));
            return;
        }
    }
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
