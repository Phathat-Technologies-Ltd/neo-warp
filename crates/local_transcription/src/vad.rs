use std::sync::Arc;

use crate::TranscribeError;

/// Sample rate every detector and the utterance stream work at.
pub const VAD_SAMPLE_RATE: u32 = 16_000;
/// Samples per detector frame (32 ms at 16 kHz).
pub const VAD_FRAME_SAMPLES: usize = 512;
const FRAME_MS: u32 = (VAD_FRAME_SAMPLES as u32 * 1000) / VAD_SAMPLE_RATE;

/// Scores how likely a frame of 16 kHz mono audio is to hold speech. Detectors may keep state
/// across frames, so use one per audio stream. Implementations are blocking but fast, well under
/// a frame's duration.
pub trait VoiceActivityDetector: Send {
    /// The probability in `[0, 1]` that `frame` (exactly [`VAD_FRAME_SAMPLES`] samples) is speech.
    fn speech_probability(&mut self, frame: &[f32]) -> Result<f32, TranscribeError>;

    /// Forgets state carried from earlier frames.
    fn reset(&mut self);
}

/// Creates a fresh detector for each audio stream.
pub type VadFactory =
    Arc<dyn Fn() -> Result<Box<dyn VoiceActivityDetector>, TranscribeError> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmenterConfig {
    /// Probability at or above which a frame counts as speech.
    pub threshold: f32,
    /// Probability below which a frame starts counting as silence. Frames between this and
    /// `threshold` neither start nor reset the silence count.
    pub silence_threshold: f32,
    /// Silence that ends an utterance; shorter pauses keep it open.
    pub min_silence_ms: u32,
    /// Speech needed before an utterance starts, so single clicks and key presses are ignored.
    pub min_speech_ms: u32,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            silence_threshold: 0.35,
            min_silence_ms: 300,
            min_speech_ms: 64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentEvent {
    SpeechStart,
    SpeechEnd,
}

/// Turns per-frame speech probabilities into utterance boundaries.
#[derive(Debug, Clone)]
pub struct Segmenter {
    config: SegmenterConfig,
    in_speech: bool,
    speech_frames: u32,
    silence_frames: Option<u32>,
}

impl Segmenter {
    pub fn new(config: SegmenterConfig) -> Self {
        Self {
            config,
            in_speech: false,
            speech_frames: 0,
            silence_frames: None,
        }
    }

    pub fn set_min_silence_ms(&mut self, min_silence_ms: u32) {
        self.config.min_silence_ms = min_silence_ms;
    }

    pub fn in_speech(&self) -> bool {
        self.in_speech
    }

    /// Frames of speech needed to start an utterance.
    pub fn min_speech_frames(&self) -> u32 {
        self.config.min_speech_ms.div_ceil(FRAME_MS).max(1)
    }

    pub fn push(&mut self, probability: f32) -> Option<SegmentEvent> {
        if !self.in_speech {
            if probability >= self.config.threshold {
                self.speech_frames += 1;
                if self.speech_frames >= self.min_speech_frames() {
                    self.in_speech = true;
                    self.speech_frames = 0;
                    self.silence_frames = None;
                    return Some(SegmentEvent::SpeechStart);
                }
            } else {
                self.speech_frames = 0;
            }
            return None;
        }
        if probability >= self.config.threshold {
            self.silence_frames = None;
        } else if probability < self.config.silence_threshold || self.silence_frames.is_some() {
            let silence = self.silence_frames.get_or_insert(0);
            *silence += 1;
            if *silence * FRAME_MS >= self.config.min_silence_ms {
                self.reset();
                return Some(SegmentEvent::SpeechEnd);
            }
        }
        None
    }

    pub fn reset(&mut self) {
        self.in_speech = false;
        self.speech_frames = 0;
        self.silence_frames = None;
    }
}

#[cfg(test)]
#[path = "vad_tests.rs"]
mod tests;
