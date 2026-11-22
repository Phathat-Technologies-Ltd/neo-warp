use parakeet_rs::ort::inputs;
use parakeet_rs::ort::session::Session;
use parakeet_rs::ort::value::Tensor;

use crate::vad::{VAD_FRAME_SAMPLES, VAD_SAMPLE_RATE, VoiceActivityDetector};
use crate::{ModelStore, TranscribeError};

/// The model file in [`crate::silero_vad_v5`].
pub const SILERO_VAD_FILE: &str = "silero_vad.onnx";
/// Samples of the previous frame that Silero v5 expects in front of each frame at 16 kHz.
const CONTEXT_SAMPLES: usize = 64;
const STATE_LEN: usize = 2 * 128;

/// Silero VAD v5 on ONNX Runtime (CPU).
pub struct SileroVad {
    session: Session,
    state: Vec<f32>,
    context: Vec<f32>,
}

fn engine_error(error: impl std::fmt::Display) -> TranscribeError {
    TranscribeError::Engine(error.to_string())
}

impl SileroVad {
    /// Loads the model from `store`.
    pub fn load(store: &ModelStore) -> Result<Self, TranscribeError> {
        if !store.is_installed() {
            return Err(TranscribeError::ModelNotInstalled);
        }
        let environment = crate::ort_env::environment()?;
        let session = Session::builder(&environment)
            .and_then(|mut builder| {
                builder = builder.with_intra_threads(1)?;
                builder.commit_from_file(store.model_dir().join(SILERO_VAD_FILE))
            })
            .map_err(engine_error)?;
        Ok(Self {
            session,
            state: vec![0.0; STATE_LEN],
            context: vec![0.0; CONTEXT_SAMPLES],
        })
    }
}

impl VoiceActivityDetector for SileroVad {
    fn speech_probability(&mut self, frame: &[f32]) -> Result<f32, TranscribeError> {
        if frame.len() != VAD_FRAME_SAMPLES {
            return Err(TranscribeError::InvalidAudio(format!(
                "VAD frames must have {VAD_FRAME_SAMPLES} samples, not {}",
                frame.len()
            )));
        }
        let mut input = Vec::with_capacity(CONTEXT_SAMPLES + VAD_FRAME_SAMPLES);
        input.extend_from_slice(&self.context);
        input.extend_from_slice(frame);
        self.context
            .copy_from_slice(&frame[VAD_FRAME_SAMPLES - CONTEXT_SAMPLES..]);
        let input = Tensor::from_array(([1, input.len()], input)).map_err(engine_error)?;
        let state = Tensor::from_array(([2, 1, 128], self.state.clone())).map_err(engine_error)?;
        let sample_rate =
            Tensor::from_array(((), vec![i64::from(VAD_SAMPLE_RATE)])).map_err(engine_error)?;
        let outputs = self
            .session
            .run(inputs!["input" => input, "state" => state, "sr" => sample_rate])
            .map_err(engine_error)?;
        let (_, probability) = outputs["output"]
            .try_extract_tensor::<f32>()
            .map_err(engine_error)?;
        let probability = probability.first().copied().unwrap_or(0.0);
        let (_, state) = outputs["stateN"]
            .try_extract_tensor::<f32>()
            .map_err(engine_error)?;
        if state.len() == STATE_LEN {
            self.state.copy_from_slice(state);
        }
        Ok(probability)
    }

    fn reset(&mut self) {
        self.state.fill(0.0);
        self.context.fill(0.0);
    }
}

#[cfg(test)]
#[path = "silero_tests.rs"]
mod tests;
