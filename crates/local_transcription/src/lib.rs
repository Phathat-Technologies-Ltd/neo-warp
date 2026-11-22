//! Speech-to-text that runs on the user's machine: a [`Transcriber`] interface that engines
//! implement, a [`ModelStore`] that downloads and verifies model files on demand, and the
//! [`ModelState`] machine that a UI drives to ask the user before downloading.
//!
//! The crate has no dependency on Warp's code so it can be reused as is.

mod audio;
mod model;
#[cfg(feature = "parakeet")]
mod parakeet;
mod state;

pub use audio::{Audio, decode_wav};
pub use model::{
    DownloadError, DownloadProgress, ModelFile, ModelSpec, ModelStatus, ModelStore,
    ProgressThrottle, parakeet_tdt_0_6b_v3,
};
#[cfg(feature = "parakeet")]
pub use parakeet::ParakeetTranscriber;
pub use state::{ModelAction, ModelEvent, ModelState};

#[derive(Debug, thiserror::Error)]
pub enum TranscribeError {
    /// The engine's model isn't on disk; the user has to download it first.
    #[error("the transcription model isn't downloaded")]
    ModelNotInstalled,
    #[error("invalid audio: {0}")]
    InvalidAudio(String),
    #[error("transcription failed: {0}")]
    Engine(String),
}

/// A speech-to-text engine. Implementations are blocking and CPU-bound; call them off async
/// executors (e.g. `tokio::task::spawn_blocking`).
pub trait Transcriber: Send + Sync {
    /// Transcribes `audio`. `language` is an optional ISO-639-1 hint; engines that detect the
    /// language themselves may ignore it.
    fn transcribe(&self, audio: &Audio, language: Option<&str>) -> Result<String, TranscribeError>;

    /// Whether the engine can transcribe now, i.e. its model is installed.
    fn is_ready(&self) -> bool;
}
