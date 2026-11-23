use std::sync::{Arc, Mutex};

use crate::bias::{BiasConfig, BiasTerm, BoostingTree};
use crate::tdt::{Acceleration, TdtModel};
use crate::{Audio, ModelStore, TranscribeError, Transcriber};

/// Asks, before each transcription, where the encoder should run.
pub type AccelerationSource = Arc<dyn Fn() -> Acceleration + Send + Sync>;

/// NVIDIA Parakeet TDT 0.6B v3 on ONNX Runtime. Loads the model from its [`ModelStore`] on
/// first use and keeps it loaded; if the files are removed it unloads and reports
/// [`TranscribeError::ModelNotInstalled`].
pub struct ParakeetTranscriber {
    store: ModelStore,
    /// The loaded model and the acceleration it was loaded with.
    model: Mutex<Option<(TdtModel, Acceleration)>>,
    acceleration: AccelerationSource,
    /// The boosting tree of the last term list, rebuilt only when the list changes.
    bias: Mutex<Option<(Vec<BiasTerm>, Arc<BoostingTree>)>>,
    bias_config: BiasConfig,
}

impl ParakeetTranscriber {
    pub fn new(store: ModelStore) -> Self {
        Self::with_bias_config(store, BiasConfig::default())
    }

    pub fn with_bias_config(store: ModelStore, bias_config: BiasConfig) -> Self {
        Self {
            store,
            model: Mutex::new(None),
            acceleration: Arc::new(Acceleration::default),
            bias: Mutex::new(None),
            bias_config,
        }
    }

    /// Reloads the model whenever `acceleration` changes.
    pub fn with_acceleration(mut self, acceleration: AccelerationSource) -> Self {
        self.acceleration = acceleration;
        self
    }

    fn run(&self, audio: &Audio, terms: &[BiasTerm]) -> Result<String, TranscribeError> {
        let mut model = self
            .model
            .lock()
            .map_err(|_| TranscribeError::Engine("the model lock is poisoned".to_owned()))?;
        if !self.store.is_installed() {
            *model = None;
            return Err(TranscribeError::ModelNotInstalled);
        }
        if audio.sample_rate != 16_000 {
            return Err(TranscribeError::InvalidAudio(format!(
                "expected 16 kHz audio, got {} Hz",
                audio.sample_rate
            )));
        }
        let acceleration = (self.acceleration)();
        if model
            .as_ref()
            .is_none_or(|(_, loaded)| *loaded != acceleration)
        {
            *model = None;
            let loaded = TdtModel::load_accelerated(self.store.model_dir(), acceleration, None)?;
            log::info!("Parakeet loaded ({acceleration:?})");
            *model = Some((loaded, acceleration));
        }
        let Some((model, _)) = model.as_mut() else {
            return Err(TranscribeError::ModelNotInstalled);
        };
        let tree = (!terms.is_empty()).then(|| self.tree(terms));
        let decoded = model.transcribe(&audio.samples, tree.as_deref())?;
        Ok(decoded.text)
    }

    fn tree(&self, terms: &[BiasTerm]) -> Arc<BoostingTree> {
        let mut cached = self.bias.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((cached_terms, tree)) = cached.as_ref()
            && cached_terms == terms
        {
            return tree.clone();
        }
        let tree = Arc::new(BoostingTree::build(terms, self.bias_config));
        *cached = Some((terms.to_vec(), tree.clone()));
        tree
    }
}

impl Transcriber for ParakeetTranscriber {
    fn transcribe(
        &self,
        audio: &Audio,
        _language: Option<&str>,
    ) -> Result<String, TranscribeError> {
        self.run(audio, &[])
    }

    fn transcribe_with_context(
        &self,
        audio: &Audio,
        _language: Option<&str>,
        terms: &[BiasTerm],
    ) -> Result<String, TranscribeError> {
        self.run(audio, terms)
    }

    fn is_ready(&self) -> bool {
        self.store.is_installed()
    }
}

#[cfg(test)]
#[path = "parakeet_tests.rs"]
mod tests;
