use std::path::Path;

use parakeet_rs::ort::session::Session;
use parakeet_rs::ort::value::Tensor;
use parakeet_rs::ort::{ep, inputs};

use crate::fbank::{FBANK_BINS, compute_fbank};
use crate::speaker::{SpeakerEmbedder, embedding_clip, l2_normalize};
use crate::{ModelStore, TranscribeError};

/// The model file in [`crate::campplus_en_voxceleb`].
pub const SPEAKER_MODEL_FILE: &str = "campplus_en_voxceleb_16k.onnx";

/// A speaker embedding model on ONNX Runtime (CPU, one thread) that takes mean-normalized fbank
/// features `x: [1, T, 80]` and returns `embedding: [1, D]`, as the 3D-Speaker and WeSpeaker
/// exports from sherpa-onnx do.
pub struct OnnxSpeakerEmbedder {
    session: Session,
}

fn engine_error(error: impl std::fmt::Display) -> TranscribeError {
    TranscribeError::Engine(error.to_string())
}

impl OnnxSpeakerEmbedder {
    /// Loads [`SPEAKER_MODEL_FILE`] from `store`.
    pub fn load(store: &ModelStore) -> Result<Self, TranscribeError> {
        if !store.is_installed() {
            return Err(TranscribeError::ModelNotInstalled);
        }
        Self::load_file(&store.model_dir().join(SPEAKER_MODEL_FILE))
    }

    pub fn load_file(path: &Path) -> Result<Self, TranscribeError> {
        let environment = crate::ort_env::environment()?;
        let session = Session::builder(&environment)
            .and_then(|mut builder| {
                builder = builder.with_intra_threads(1)?;
                builder = builder.with_inter_threads(1)?;
                // One small request per utterance: skip the arena and memory-pattern caches,
                // which keep tens of megabytes alive between runs.
                builder = builder.with_memory_pattern(false)?;
                builder = builder.with_prepacking(false)?;
                builder = builder.with_execution_providers([ep::CPU::default()
                    .with_arena_allocator(false)
                    .build()])?;
                builder.commit_from_file(path)
            })
            .map_err(engine_error)?;
        Ok(Self { session })
    }
}

impl SpeakerEmbedder for OnnxSpeakerEmbedder {
    fn embed(&mut self, samples: &[f32]) -> Result<Vec<f32>, TranscribeError> {
        let clip = embedding_clip(samples);
        let mut features = compute_fbank(&clip);
        if features.frames == 0 {
            return Err(TranscribeError::InvalidAudio(
                "no speech to identify".to_owned(),
            ));
        }
        features.subtract_mean();
        let input = Tensor::from_array(([1, features.frames, FBANK_BINS], features.data))
            .map_err(engine_error)?;
        let outputs = self
            .session
            .run(inputs!["x" => input])
            .map_err(engine_error)?;
        let (_, embedding) = outputs["embedding"]
            .try_extract_tensor::<f32>()
            .map_err(engine_error)?;
        let mut embedding = embedding.to_vec();
        l2_normalize(&mut embedding);
        Ok(embedding)
    }
}

#[cfg(test)]
#[path = "speaker_onnx_tests.rs"]
mod tests;
