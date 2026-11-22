//! Full-duplex audio with acoustic echo cancellation, so the microphone doesn't hear speech played
//! on loudspeakers. On macOS this is Apple's voice-processing I/O unit, which owns both the
//! microphone and the speaker; elsewhere (or without the `voice_processing` feature) it is never
//! available and callers use their cpal streams.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;

use parking_lot::Mutex;

use crate::recovery::Signal;

#[cfg(all(target_os = "macos", feature = "voice_processing"))]
mod macos;
#[cfg(all(target_os = "macos", feature = "voice_processing"))]
pub(crate) use macos::{CaptureLease, OutputLease, acquire_capture, acquire_output};

#[cfg(not(all(target_os = "macos", feature = "voice_processing")))]
mod unsupported;
#[cfg(not(all(target_os = "macos", feature = "voice_processing")))]
pub(crate) use unsupported::{CaptureLease, OutputLease, acquire_capture, acquire_output};

/// Sample rate of microphone audio handed to capture sinks.
pub(crate) const CAPTURE_RATE: f64 = 16_000.0;
/// Sample rate requested for speech sent to the speaker; the unit may pick another.
#[cfg_attr(
    not(all(target_os = "macos", feature = "voice_processing")),
    allow(dead_code)
)]
pub(crate) const PLAYBACK_RATE: f64 = 24_000.0;
/// Consecutive failed starts after which the voice-processing backend is skipped until it is
/// wanted again, so a broken unit doesn't add a failed start to every microphone rebuild.
const MAX_START_FAILURES: u32 = 2;

/// Mono `f32` microphone samples at [`CAPTURE_RATE`], called from the audio thread.
pub(crate) type CaptureSink = Box<dyn FnMut(&[f32]) + Send>;
pub(crate) type OnFinished = Box<dyn FnOnce() + Send>;

/// Which implementation a microphone or speaker stream uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioBackend {
    /// Plain cpal streams with no echo cancellation.
    Cpal,
    /// The shared echo-cancelling unit.
    VoiceProcessing,
}

/// Decides whether streams try the voice-processing backend, and backs off after it fails.
#[derive(Debug, Clone, Default)]
pub struct BackendSelector {
    wanted: bool,
    failures: u32,
}

impl BackendSelector {
    /// Returns whether the preference changed. Wanting it again forgets earlier failures.
    pub fn set_wanted(&mut self, wanted: bool) -> bool {
        let changed = self.wanted != wanted;
        self.wanted = wanted;
        if changed {
            self.failures = 0;
        }
        changed
    }

    pub fn choose(&self, supported: bool) -> AudioBackend {
        if self.wanted && supported && self.failures < MAX_START_FAILURES {
            AudioBackend::VoiceProcessing
        } else {
            AudioBackend::Cpal
        }
    }

    pub fn on_started(&mut self, backend: AudioBackend) {
        if backend == AudioBackend::VoiceProcessing {
            self.failures = 0;
        }
    }

    pub fn on_start_failed(&mut self) {
        self.failures += 1;
    }
}

/// Opens a stream on the backend `selector` picks, falling back to cpal when the voice-processing
/// backend fails to start.
pub fn open_with_fallback<S>(
    selector: &mut BackendSelector,
    supported: bool,
    open_voice_processing: impl FnOnce() -> anyhow::Result<S>,
    open_cpal: impl FnOnce() -> anyhow::Result<S>,
) -> anyhow::Result<(S, AudioBackend)> {
    if selector.choose(supported) == AudioBackend::VoiceProcessing {
        match open_voice_processing() {
            Ok(stream) => {
                selector.on_started(AudioBackend::VoiceProcessing);
                return Ok((stream, AudioBackend::VoiceProcessing));
            }
            Err(error) => {
                log::warn!(
                    "Echo-cancelling audio unavailable, using the plain audio path: {error:#}"
                );
                selector.on_start_failed();
            }
        }
    }
    Ok((open_cpal()?, AudioBackend::Cpal))
}

static SELECTOR: Mutex<BackendSelector> = Mutex::new(BackendSelector {
    wanted: false,
    failures: 0,
});
/// Supervisors of open microphones, told to reopen when the preferred backend changes.
static CAPTURES: Mutex<Vec<(u64, mpsc::Sender<Signal>)>> = Mutex::new(Vec::new());
static NEXT_CAPTURE_ID: AtomicU64 = AtomicU64::new(0);
/// Whether the unit is running with both a microphone and a speaker attached.
#[cfg_attr(
    not(all(target_os = "macos", feature = "voice_processing")),
    allow(dead_code)
)]
pub(crate) static ECHO_CANCELLING: AtomicBool = AtomicBool::new(false);

/// Whether this build can use the voice-processing backend at all.
pub fn is_supported() -> bool {
    cfg!(all(target_os = "macos", feature = "voice_processing"))
}

/// Sets whether microphones and speakers opened from now on use the echo-cancelling backend.
/// Open microphones are reopened on the new backend.
pub fn set_voice_processing_wanted(wanted: bool) {
    if !SELECTOR.lock().set_wanted(wanted) {
        return;
    }
    log::info!(
        "Echo-cancelling audio {}",
        if wanted { "wanted" } else { "off" }
    );
    reopen_captures();
}

/// Whether the microphone signal currently has acoustic echo cancellation for the speech being
/// played: the shared unit runs with both the microphone and the speaker on it.
pub fn is_echo_cancelling() -> bool {
    ECHO_CANCELLING.load(Ordering::Relaxed)
}

pub(crate) fn with_selector<R>(f: impl FnOnce(&mut BackendSelector) -> R) -> R {
    f(&mut SELECTOR.lock())
}

pub(crate) fn reopen_captures() {
    for (_, tx) in CAPTURES.lock().iter() {
        let _ = tx.send(Signal::Reopen);
    }
}

/// Keeps a microphone supervisor registered for [`Signal::Reopen`] until dropped.
pub(crate) struct CaptureRegistration(u64);

impl CaptureRegistration {
    pub(crate) fn new(tx: mpsc::Sender<Signal>) -> Self {
        let id = NEXT_CAPTURE_ID.fetch_add(1, Ordering::Relaxed);
        CAPTURES.lock().push((id, tx));
        Self(id)
    }
}

impl Drop for CaptureRegistration {
    fn drop(&mut self) {
        CAPTURES.lock().retain(|(id, _)| *id != self.0);
    }
}

/// A fixed-capacity single-producer single-consumer queue of samples whose consumer side never
/// blocks or allocates, for feeding a real-time audio callback. Positions only grow; the producer
/// can discard everything queued with [`Self::flush`].
pub struct SampleRing {
    samples: Box<[AtomicU32]>,
    mask: usize,
    write: AtomicUsize,
    read: AtomicUsize,
    /// A read position the consumer jumps to on its next pop, or `usize::MAX` for none.
    flush_to: AtomicUsize,
}

impl SampleRing {
    /// Creates a ring holding at least `min_capacity` samples.
    pub fn new(min_capacity: usize) -> Self {
        let capacity = min_capacity.max(1).next_power_of_two();
        Self {
            samples: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
            mask: capacity - 1,
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            flush_to: AtomicUsize::new(usize::MAX),
        }
    }

    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    fn effective_read(&self) -> usize {
        let read = self.read.load(Ordering::Acquire);
        match self.flush_to.load(Ordering::Acquire) {
            usize::MAX => read,
            flush_to => read.max(flush_to),
        }
    }

    /// Producer: queues as many of `samples` as fit and returns how many were queued.
    pub fn push(&self, samples: &[f32]) -> usize {
        let write = self.write.load(Ordering::Relaxed);
        let free = self.capacity() - (write - self.effective_read());
        let count = samples.len().min(free);
        for (offset, &sample) in samples[..count].iter().enumerate() {
            self.samples[(write + offset) & self.mask].store(sample.to_bits(), Ordering::Relaxed);
        }
        self.write.store(write + count, Ordering::Release);
        count
    }

    /// Producer: discards everything queued so far.
    pub fn flush(&self) {
        self.flush_to
            .store(self.write.load(Ordering::Relaxed), Ordering::Release);
    }

    /// Producer: the position just past the last queued sample.
    pub fn write_position(&self) -> usize {
        self.write.load(Ordering::Relaxed)
    }

    /// Consumer: fills `out` with queued samples, padding with silence, and returns the read
    /// position afterwards.
    pub fn pop_into(&self, out: &mut [f32]) -> usize {
        let mut read = self.read.load(Ordering::Relaxed);
        let flush_to = self.flush_to.swap(usize::MAX, Ordering::Acquire);
        if flush_to != usize::MAX {
            read = read.max(flush_to);
        }
        let available = self.write.load(Ordering::Acquire) - read;
        let count = available.min(out.len());
        for (offset, slot) in out[..count].iter_mut().enumerate() {
            *slot =
                f32::from_bits(self.samples[(read + offset) & self.mask].load(Ordering::Relaxed));
        }
        out[count..].fill(0.0);
        read += count;
        self.read.store(read, Ordering::Release);
        read
    }
}

/// Speech queued for the unit's speaker, shared by the producer ([`OutputLease`]) and the render
/// callback.
pub(crate) struct PlaybackQueue {
    ring: SampleRing,
    /// The ring position where the current clip ends, and what to call once it has played.
    finished: Mutex<Option<(usize, OnFinished)>>,
}

impl PlaybackQueue {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            ring: SampleRing::new(capacity),
            finished: Mutex::new(None),
        }
    }

    /// Replaces anything queued with `samples`. `on_finished` runs from [`Self::render`] once the
    /// last sample has been played.
    pub(crate) fn play(&self, samples: &[f32], on_finished: OnFinished) {
        let mut finished = self.finished.lock();
        *finished = None;
        self.ring.flush();
        let queued = self.ring.push(samples);
        if queued < samples.len() {
            log::warn!(
                "Speech clip of {} samples truncated to {queued} for the echo-cancelling output",
                samples.len()
            );
        }
        *finished = Some((self.ring.write_position(), on_finished));
    }

    pub(crate) fn stop(&self) {
        let mut finished = self.finished.lock();
        *finished = None;
        self.ring.flush();
    }

    /// Fills `out` from the audio thread without blocking.
    pub(crate) fn render(&self, out: &mut [f32]) {
        let read = self.ring.pop_into(out);
        // A producer holding the lock is replacing the clip; check again on the next callback.
        let Some(mut finished) = self.finished.try_lock() else {
            return;
        };
        if finished.as_ref().is_some_and(|(end, _)| read >= *end)
            && let Some((_, on_finished)) = finished.take()
        {
            drop(finished);
            on_finished();
        }
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
