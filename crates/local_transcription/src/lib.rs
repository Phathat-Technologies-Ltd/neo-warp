//! Speech-to-text that runs on the user's machine: a [`Transcriber`] interface that engines
//! implement, a [`ModelStore`] that downloads and verifies model files on demand, and the
//! [`ModelState`] machine that a UI drives to ask the user before downloading. For hands-free
//! input, a [`VoiceActivityDetector`] and [`stream::UtteranceStream`] split live audio into
//! utterances. For conversation mode, [`speech::Speaker`] engines read agent replies aloud.
//! [`speaker`] verifies that an utterance came from the enrolled user.
//!
//! The crate has no dependency on Warp's code so it can be reused as is.

mod audio;
#[cfg(feature = "denoise")]
pub mod denoise;
pub mod fbank;
mod model;
#[cfg(any(
    feature = "parakeet",
    feature = "silero",
    feature = "kokoro",
    feature = "tiny_voca",
    feature = "speaker_id"
))]
mod ort_env;
#[cfg(feature = "parakeet")]
mod parakeet;
#[cfg(feature = "silero")]
mod silero;
pub mod speaker;
#[cfg(feature = "speaker_id")]
mod speaker_onnx;
pub mod speech;
mod state;
pub mod stream;
pub mod terminal_text;
mod vad;
mod voice_command;

pub use audio::{Audio, decode_wav};
pub use model::{
    DownloadError, DownloadProgress, ModelBundle, ModelFile, ModelSpec, ModelStatus, ModelStore,
    ProgressThrottle, campplus_en_voxceleb, kokoro_82m_v1_int8, parakeet_tdt_0_6b_v3,
    silero_vad_v5, tiny_voca,
};
#[cfg(feature = "speaker_id")]
pub use speaker_onnx::{OnnxSpeakerEmbedder, SPEAKER_MODEL_FILE};
#[cfg(feature = "parakeet")]
pub use parakeet::ParakeetTranscriber;
#[cfg(feature = "silero")]
pub use silero::SileroVad;
pub use state::{ModelAction, ModelEvent, ModelState};
pub use vad::{
    SegmentEvent, Segmenter, SegmenterConfig, VAD_FRAME_SAMPLES, VAD_SAMPLE_RATE, VadFactory,
    VoiceActivityDetector,
};
pub use voice_command::{VoiceCommand, split_voice_command};

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
