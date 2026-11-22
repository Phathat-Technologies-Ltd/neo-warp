//! Terminal session recordings: the asciicast v2 format, secret redaction, and storage of
//! recordings behind [`RecordingStore`] so the backend can be local or remote.

pub mod asciicast;
mod local;
pub mod naming;
pub mod redact;
pub mod sanitize;
mod store;

pub use local::LocalDirStore;
pub use store::{
    AppendRequest, ByteRange, CreateRecording, CreatedRecording, RecordingContent,
    RecordingMetadata, RecordingStore, RenameRecording, StoreError, is_valid_id,
};
