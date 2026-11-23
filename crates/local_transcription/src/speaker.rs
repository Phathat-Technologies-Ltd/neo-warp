//! Speaker verification: a [`SpeakerEmbedder`] turns an utterance into a voice embedding, an
//! enrollment averages a few into a [`Voiceprint`], and later utterances are accepted when their
//! cosine similarity to it clears the [`Sensitivity`] threshold.

use serde::{Deserialize, Serialize};

use crate::TranscribeError;

/// The most audio embedded per utterance; the end of a longer one is used.
pub const MAX_EMBED_SAMPLES: usize = 3 * 16_000;
/// Shorter audio is tiled up to this length, so short words still fill the model's context.
pub const MIN_EMBED_SAMPLES: usize = 16_000;
/// Phrases read during enrollment: long enough for five to eight seconds of speech each, with
/// varied vowels and consonants.
pub const ENROLLMENT_PHRASES: [&str; 6] = [
    "Hey Warp, show me what changed in this repository today, and summarize the biggest \
     differences for me.",
    "Run the whole test suite again, then tell me which tests are failing and why they broke.",
    "Open a new tab, move into my home directory, and list every file sorted by size.",
    "Could you check whether the build server is still running, and restart it if it froze?",
    "Search the project for the word timeout, then explain how each of those values is used.",
    "Thanks, that looks good. Please commit these changes with a short, clear message.",
];
/// The current [`Voiceprint::version`]; voiceprints of another version ask for re-enrollment.
pub const VOICEPRINT_VERSION: u32 = 2;
/// Live embeddings folded into a voiceprint, most recent kept.
pub const MAX_ADAPTED_EMBEDDINGS: usize = 8;

/// Turns 16 kHz mono speech into an L2-normalized voice embedding. Implementations are blocking
/// and CPU-bound.
pub trait SpeakerEmbedder: Send {
    fn embed(&mut self, samples: &[f32]) -> Result<Vec<f32>, TranscribeError>;
}

/// The audio to embed for an utterance: its last [`MAX_EMBED_SAMPLES`], tiled to at least
/// [`MIN_EMBED_SAMPLES`]. Empty when there is no audio.
pub fn embedding_clip(samples: &[f32]) -> Vec<f32> {
    let tail = &samples[samples.len().saturating_sub(MAX_EMBED_SAMPLES)..];
    if tail.is_empty() || tail.len() >= MIN_EMBED_SAMPLES {
        return tail.to_vec();
    }
    tail.iter()
        .copied()
        .cycle()
        .take(MIN_EMBED_SAMPLES)
        .collect()
}

/// Scales `vector` to unit length; a zero vector stays zero.
pub fn l2_normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        vector.iter_mut().for_each(|x| *x /= norm);
    }
}

/// Cosine similarity in `[-1, 1]`; 0 when either vector is zero or the lengths differ.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a <= f32::EPSILON || norm_b <= f32::EPSILON {
        return 0.0;
    }
    (dot / (norm_a * norm_b)).clamp(-1.0, 1.0)
}

/// How closely an utterance must match the voiceprint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    /// Fewer false accepts (other voices, loudspeaker echo), more misses of the user.
    Strict,
    #[default]
    Normal,
    /// Fewer misses of the user in noisy rooms or with a different microphone.
    Loose,
}

impl Sensitivity {
    /// The lowest cosine similarity accepted.
    pub fn threshold(self) -> f32 {
        match self {
            Sensitivity::Strict => 0.52,
            Sensitivity::Normal => 0.44,
            Sensitivity::Loose => 0.36,
        }
    }

    /// Parses a setting value, defaulting to [`Sensitivity::Normal`].
    pub fn from_setting(value: &str) -> Self {
        match value {
            "strict" => Sensitivity::Strict,
            "loose" => Sensitivity::Loose,
            _ => Sensitivity::Normal,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnrollError {
    #[error("enrollment needs at least one recording")]
    NoSamples,
    #[error("the recordings produced embeddings of different sizes")]
    MismatchedDimensions,
}

/// The enrolled user's voice: the renormalized mean of the normalized enrollment embeddings and
/// the most recent confidently matched live embeddings, so it learns the user's everyday
/// microphone and room.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Voiceprint {
    /// The [`crate::ModelSpec`] id of the embedder, so a model change asks for re-enrollment.
    pub model_id: String,
    /// Bumped when the audio pipeline or features change; see [`VOICEPRINT_VERSION`].
    #[serde(default)]
    pub version: u32,
    /// The embedding utterances are compared with.
    pub embedding: Vec<f32>,
    #[serde(default)]
    pub enrollment: Vec<Vec<f32>>,
    /// At most [`MAX_ADAPTED_EMBEDDINGS`], oldest first.
    #[serde(default)]
    pub adapted: Vec<Vec<f32>>,
}

impl Voiceprint {
    pub fn enroll(model_id: &str, embeddings: &[Vec<f32>]) -> Result<Self, EnrollError> {
        let Some(first) = embeddings.first() else {
            return Err(EnrollError::NoSamples);
        };
        let dim = first.len();
        if dim == 0 || embeddings.iter().any(|embedding| embedding.len() != dim) {
            return Err(EnrollError::MismatchedDimensions);
        }
        let enrollment = embeddings
            .iter()
            .map(|embedding| {
                let mut normalized = embedding.clone();
                l2_normalize(&mut normalized);
                normalized
            })
            .collect();
        let mut print = Self {
            model_id: model_id.to_owned(),
            version: VOICEPRINT_VERSION,
            embedding: Vec::new(),
            enrollment,
            adapted: Vec::new(),
        };
        print.recompute();
        Ok(print)
    }

    /// Whether this voiceprint was made with `model_id` and the current pipeline.
    pub fn is_current(&self, model_id: &str) -> bool {
        self.model_id == model_id && self.version == VOICEPRINT_VERSION
    }

    /// Folds a confidently matched live embedding into the voiceprint, dropping the oldest once
    /// [`MAX_ADAPTED_EMBEDDINGS`] are kept. Embeddings of another size are ignored.
    pub fn adapt(&mut self, embedding: &[f32]) {
        if embedding.len() != self.embedding.len() {
            return;
        }
        let mut normalized = embedding.to_vec();
        l2_normalize(&mut normalized);
        if self.adapted.len() == MAX_ADAPTED_EMBEDDINGS {
            self.adapted.remove(0);
        }
        self.adapted.push(normalized);
        self.recompute();
    }

    fn recompute(&mut self) {
        let Some(dim) = self.enrollment.first().map(Vec::len) else {
            return;
        };
        let mut mean = vec![0.0f32; dim];
        for embedding in self.enrollment.iter().chain(&self.adapted) {
            for (sum, value) in mean.iter_mut().zip(embedding) {
                *sum += value;
            }
        }
        l2_normalize(&mut mean);
        self.embedding = mean;
    }

    pub fn similarity(&self, embedding: &[f32]) -> f32 {
        cosine_similarity(&self.embedding, embedding)
    }

    pub fn matches(&self, embedding: &[f32], sensitivity: Sensitivity) -> bool {
        self.similarity(embedding) >= sensitivity.threshold()
    }
}

#[cfg(test)]
#[path = "speaker_tests.rs"]
mod tests;
