//! Recording terminal panes as asciicast v2 and listing and replaying the recordings. Capture and
//! redaction happen here; storage is the server's `/recordings` API.

pub mod client;
pub mod list;
pub mod player;
pub mod recorder;
pub mod recordings_view;
pub mod screen_style;
pub mod settings;

pub use ::session_recording::{asciicast, naming, redact, sanitize};
pub use settings::SessionRecordingSettings;
