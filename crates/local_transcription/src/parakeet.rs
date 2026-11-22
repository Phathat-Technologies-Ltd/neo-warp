use std::sync::Mutex;

use parakeet_rs::{ExecutionConfig, ParakeetTDT, Transcriber as _};

use crate::{Audio, ModelStore, TranscribeError, Transcriber};

/// NVIDIA Parakeet TDT 0.6B v3 on ONNX Runtime (CPU). Loads the model from its [`ModelStore`] on
/// first use and keeps it loaded; if the files are removed it unloads and reports
/// [`TranscribeError::ModelNotInstalled`].
pub struct ParakeetTranscriber {
    store: ModelStore,
    model: Mutex<Option<ParakeetTDT>>,
}

impl ParakeetTranscriber {
    pub fn new(store: ModelStore) -> Self {
        Self {
            store,
            model: Mutex::new(None),
        }
    }
}

impl Transcriber for ParakeetTranscriber {
    fn transcribe(
        &self,
        audio: &Audio,
        _language: Option<&str>,
    ) -> Result<String, TranscribeError> {
        let mut model = self
            .model
            .lock()
            .map_err(|_| TranscribeError::Engine("the model lock is poisoned".to_owned()))?;
        if !self.store.is_installed() {
            *model = None;
            return Err(TranscribeError::ModelNotInstalled);
        }
        if model.is_none() {
            let engine_error =
                |error: &dyn std::fmt::Display| TranscribeError::Engine(error.to_string());
            let environment = crate::ort_env::environment()?;
            let config = ExecutionConfig::new().with_environment(environment);
            let loaded = ParakeetTDT::from_pretrained(self.store.model_dir(), Some(config))
                .map_err(|error| engine_error(&error))?;
            *model = Some(loaded);
        }
        let Some(model) = model.as_mut() else {
            return Err(TranscribeError::ModelNotInstalled);
        };
        let result = model
            .transcribe_samples(audio.samples.clone(), audio.sample_rate, 1, None)
            .map_err(|error| TranscribeError::Engine(error.to_string()))?;
        Ok(result.text.trim().to_owned())
    }

    fn is_ready(&self) -> bool {
        self.store.is_installed()
    }
}

#[cfg(test)]
#[path = "parakeet_tests.rs"]
mod tests;
