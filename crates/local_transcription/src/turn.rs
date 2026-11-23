use std::sync::Arc;

use crate::TranscribeError;

/// Judges from the sound of an utterance whether the speaker finished their turn or only paused
/// mid-thought. Implementations are blocking; they must be fast enough to run on a pause.
pub trait TurnDetector: Send {
    /// The probability in `[0, 1]` that the speaker finished, given 16 kHz mono audio of the
    /// utterance so far, ending with the pause.
    fn completion_probability(&mut self, audio: &[f32]) -> Result<f32, TranscribeError>;
}

/// Creates a detector for each audio stream.
pub type TurnDetectorFactory =
    Arc<dyn Fn() -> Result<Box<dyn TurnDetector>, TranscribeError> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EndOfTurnConfig {
    /// Silence after which the detector is asked whether the turn is over.
    pub probe_silence_ms: u32,
    /// Probability at or above which the turn counts as finished.
    pub threshold: f32,
    /// Silence that ends the turn anyway when the detector judged it unfinished.
    pub incomplete_silence_ms: u32,
}

impl Default for EndOfTurnConfig {
    fn default() -> Self {
        Self {
            probe_silence_ms: 200,
            threshold: 0.5,
            incomplete_silence_ms: 2000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnDecision {
    /// Keep listening.
    Wait,
    /// Ask the detector now and end the turn if it says the speaker finished.
    Probe,
    /// End the turn.
    End,
}

/// Decides, frame by frame during a pause, when to consult a [`TurnDetector`] and when to end the
/// turn. Without a detector verdict, the pause ends the turn at the incomplete timeout.
#[derive(Debug, Clone)]
pub struct EndOfTurn {
    config: EndOfTurnConfig,
    probed: bool,
}

impl EndOfTurn {
    pub fn new(config: EndOfTurnConfig) -> Self {
        Self {
            config,
            probed: false,
        }
    }

    pub fn config(&self) -> EndOfTurnConfig {
        self.config
    }

    /// Advances with the current pause length, `None` while the speaker talks.
    pub fn on_silence(&mut self, silence_ms: Option<u32>) -> TurnDecision {
        let Some(silence_ms) = silence_ms else {
            self.probed = false;
            return TurnDecision::Wait;
        };
        if silence_ms >= self.config.incomplete_silence_ms {
            self.probed = false;
            return TurnDecision::End;
        }
        if !self.probed && silence_ms >= self.config.probe_silence_ms {
            self.probed = true;
            return TurnDecision::Probe;
        }
        TurnDecision::Wait
    }

    /// Whether the detector's `probability` ends the turn.
    pub fn is_complete(&self, probability: f32) -> bool {
        probability >= self.config.threshold
    }

    pub fn reset(&mut self) {
        self.probed = false;
    }
}

/// Words an English request rarely ends on; a transcript ending on one was cut off mid-thought.
const CONTINUATION_WORDS: &[&str] = &[
    "a", "an", "the", "to", "in", "on", "of", "for", "from", "with", "into", "about", "at", "by",
    "and", "or", "but", "so", "because", "that", "which", "whether", "if", "then", "than", "is",
    "are", "was", "my", "our", "your", "this", "these", "called", "named", "um", "uh", "like",
];

/// Whether `transcript` ends on a word that implies more is coming, such as "list the files in".
pub fn ends_mid_thought(transcript: &str) -> bool {
    let trimmed = transcript.trim_end();
    if trimmed.ends_with(['.', '?', '!']) && !trimmed.ends_with("...") {
        return false;
    }
    trimmed
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .rsplit(|c: char| !c.is_alphanumeric() && c != '\'')
        .next()
        .is_some_and(|word| CONTINUATION_WORDS.contains(&word.to_lowercase().as_str()))
}

#[cfg(test)]
#[path = "turn_tests.rs"]
mod tests;
