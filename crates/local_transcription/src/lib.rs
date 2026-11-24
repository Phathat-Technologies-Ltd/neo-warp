//! Speech-to-text that runs on the user's machine: a [`Transcriber`] interface that engines
//! implement, a [`ModelStore`] that downloads and verifies model files on demand, and the
//! [`ModelState`] machine that a UI drives to ask the user before downloading. For hands-free
//! input, a [`VoiceActivityDetector`] and [`stream::UtteranceStream`] split live audio into
//! utterances. For conversation mode, [`speech::Speaker`] engines read agent replies aloud.
//! [`speaker`] verifies that an utterance came from the enrolled user.
//!
//! The crate has no dependency on Warp's code so it can be reused as is.

mod audio;
pub mod bias;
pub mod context_correction;
#[cfg(feature = "denoise")]
pub mod denoise;
pub mod disfluency;
#[cfg(feature = "disfluency")]
mod disfluency_onnx;
pub mod fbank;
pub mod filler;
mod model;
#[cfg(any(
    feature = "parakeet",
    feature = "silero",
    feature = "kokoro",
    feature = "tiny_voca",
    feature = "pocket",
    feature = "speaker_id",
    feature = "smart_turn",
    feature = "disfluency"
))]
mod ort_env;
#[cfg(feature = "parakeet")]
mod parakeet;
#[cfg(feature = "silero")]
mod silero;
#[cfg(feature = "smart_turn")]
mod smart_turn;
pub mod speaker;
#[cfg(feature = "speaker_id")]
mod speaker_onnx;
pub mod speech;
mod state;
pub mod stream;
#[cfg(feature = "parakeet")]
pub mod tdt;
pub mod terminal_text;
pub mod turn;
mod vad;
mod voice_command;
pub mod whisper_mel;

pub use audio::{Audio, decode_wav};
#[cfg(feature = "disfluency")]
pub use disfluency_onnx::DisfluencyTagger;
pub use model::{
    DownloadError, DownloadProgress, ModelBundle, ModelFile, ModelSpec, ModelStatus, ModelStore,
    ProgressThrottle, campplus_zh_en_advanced, kokoro_82m_v1_int8, parakeet_fp16_encoder,
    parakeet_tdt_0_6b_v3, pocket_tts_int8, silero_vad_v5, smart_turn_v3_2, tiny_voca,
};
#[cfg(feature = "parakeet")]
pub use parakeet::{AccelerationSource, FillerSource, ParakeetTranscriber};
#[cfg(feature = "silero")]
pub use silero::SileroVad;
#[cfg(feature = "smart_turn")]
pub use smart_turn::{SMART_TURN_FILE, SmartTurnDetector, smart_turn_features};
#[cfg(feature = "speaker_id")]
pub use speaker_onnx::{OnnxSpeakerEmbedder, SPEAKER_MODEL_FILE};
pub use state::{ModelAction, ModelEvent, ModelState};
pub use turn::{
    EndOfTurn, EndOfTurnConfig, TurnDecision, TurnDetector, TurnDetectorFactory, ends_mid_thought,
};
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

    /// Transcribes `audio` biased toward `terms` the user is likely to say. Engines that can't
    /// bias their decoding transcribe as [`Self::transcribe`] does.
    fn transcribe_with_context(
        &self,
        audio: &Audio,
        language: Option<&str>,
        terms: &[bias::BiasTerm],
    ) -> Result<String, TranscribeError> {
        let _ = terms;
        self.transcribe(audio, language)
    }

    /// Whether the engine can transcribe now, i.e. its model is installed.
    fn is_ready(&self) -> bool;
}
