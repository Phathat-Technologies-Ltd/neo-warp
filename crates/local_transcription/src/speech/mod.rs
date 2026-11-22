//! Text-to-speech for voice conversation mode: a [`Speaker`] interface that engines implement, and
//! the pure logic that decides what of an agent reply is spoken and when.

pub mod approval;
pub mod chunker;
#[cfg(any(feature = "kokoro", feature = "tiny_voca"))]
pub mod g2p;
#[cfg(feature = "kokoro")]
mod kokoro;
pub mod playback;
pub mod spoken;
pub mod summary;
#[cfg(feature = "tiny_voca")]
mod tiny_voca;
pub mod tts_text;

#[cfg(feature = "kokoro")]
pub use kokoro::{KOKORO_VOICES, KokoroSpeaker};
#[cfg(feature = "tiny_voca")]
pub use tiny_voca::{TINY_VOCA_VOICES, TinyVocaSpeaker};

use crate::Audio;

/// A voice of a speech engine, such as `af_heart`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VoiceId(pub String);

impl VoiceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A voice a user can pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceInfo {
    pub id: VoiceId,
    pub display_name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SpeakError {
    /// The engine's model isn't on disk; the user has to download it first.
    #[error("the speech output model isn't downloaded")]
    ModelNotInstalled,
    #[error("unknown voice {0}")]
    UnknownVoice(String),
    #[error("speech synthesis failed: {0}")]
    Engine(String),
}

/// The slowest and fastest speaking rates offered.
pub const MIN_RATE: f32 = 0.8;
pub const MAX_RATE: f32 = 1.5;

/// A text-to-speech engine. Implementations are blocking and CPU-bound; call them off async
/// executors (e.g. `spawn_blocking`).
pub trait Speaker: Send + Sync {
    /// Speaks `text`, which is already normalized for speech (see [`tts_text::normalize`]), in
    /// `voice` at `rate` times the normal speed.
    fn synthesize(&self, text: &str, voice: &VoiceId, rate: f32) -> Result<Audio, SpeakError>;

    /// Whether the engine can speak now, i.e. its model is installed.
    fn is_ready(&self) -> bool;

    /// The voices the engine offers, the default first.
    fn voices(&self) -> Vec<VoiceInfo>;
}
