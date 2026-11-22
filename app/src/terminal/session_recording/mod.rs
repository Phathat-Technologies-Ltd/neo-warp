//! Recording terminal panes as asciicast v2 and listing and replaying the recordings. Capture and
//! redaction happen here; storage is the server's `/recordings` API.

pub mod client;
pub mod player;
pub mod recorder;
pub mod settings;

pub use ::session_recording::{asciicast, naming, redact};
pub use settings::SessionRecordingSettings;
