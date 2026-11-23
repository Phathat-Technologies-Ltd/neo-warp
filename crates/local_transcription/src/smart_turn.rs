use parakeet_rs::ort::inputs;
use parakeet_rs::ort::session::Session;
use parakeet_rs::ort::value::Tensor;

use crate::turn::TurnDetector;
use crate::vad::VAD_SAMPLE_RATE;
use crate::whisper_mel::{WHISPER_MEL_BINS, normalize, whisper_log_mel};
use crate::{ModelStore, TranscribeError};

/// The model file in [`crate::smart_turn_v3_2`].
pub const SMART_TURN_FILE: &str = "smart-turn-v3.2-cpu.onnx";
/// The model hears the last 8 s of the utterance; shorter audio is zero-padded in front.
const WINDOW_SAMPLES: usize = 8 * VAD_SAMPLE_RATE as usize;
const FRAMES: usize = WINDOW_SAMPLES / 160;

/// Pipecat Smart Turn v3.2 (BSD-2-Clause) on ONNX Runtime (CPU).
pub struct SmartTurnDetector {
    session: Session,
}

fn engine_error(error: impl std::fmt::Display) -> TranscribeError {
    TranscribeError::Engine(error.to_string())
}

impl SmartTurnDetector {
    pub fn load(store: &ModelStore) -> Result<Self, TranscribeError> {
        if !store.is_installed() {
            return Err(TranscribeError::ModelNotInstalled);
        }
        let environment = crate::ort_env::environment()?;
        let session = Session::builder(&environment)
            .and_then(|mut builder| {
                // The memory pattern holds about 25 MB more between runs for no measurable speedup.
                builder = builder.with_intra_threads(1)?.with_memory_pattern(false)?;
                builder.commit_from_file(store.model_dir().join(SMART_TURN_FILE))
            })
            .map_err(engine_error)?;
        Ok(Self { session })
    }
}

/// The model's input for `audio`: its last 8 s, front-padded with silence, normalized.
pub fn smart_turn_features(audio: &[f32]) -> Vec<f32> {
    let mut window = vec![0.0f32; WINDOW_SAMPLES];
    let tail = &audio[audio.len().saturating_sub(WINDOW_SAMPLES)..];
    window[WINDOW_SAMPLES - tail.len()..].copy_from_slice(tail);
    normalize(&mut window);
    whisper_log_mel(&window)
}

impl TurnDetector for SmartTurnDetector {
    fn completion_probability(&mut self, audio: &[f32]) -> Result<f32, TranscribeError> {
        let features = smart_turn_features(audio);
        let input =
            Tensor::from_array(([1, WHISPER_MEL_BINS, FRAMES], features)).map_err(engine_error)?;
        let outputs = self
            .session
            .run(inputs!["input_features" => input])
            .map_err(engine_error)?;
        let (_, probability) = outputs["logits"]
            .try_extract_tensor::<f32>()
            .map_err(engine_error)?;
        Ok(probability.first().copied().unwrap_or(0.0))
    }
}
