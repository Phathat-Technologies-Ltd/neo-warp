//! The storage interface for recordings, and the types exchanged over the recordings API.

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::asciicast::Header;

/// Body of `POST /recordings`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateRecording {
    pub header: Header,
    /// Used to name the recording; sanitized by the store.
    pub name: String,
}

/// Response to `POST /recordings`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreatedRecording {
    pub id: String,
}

/// Query of `POST /recordings/{id}/events`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppendRequest {
    /// Batches of one recording are numbered from 0; a batch already stored is acknowledged
    /// without being written again, so a client can safely resend after a lost response.
    pub seq: u64,
}

/// Body of `PATCH /recordings/{id}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenameRecording {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordingMetadata {
    pub id: String,
    pub title: String,
    /// Unix time, in seconds.
    pub started_at: Option<i64>,
    /// Seconds.
    pub duration: f64,
    pub size_bytes: u64,
    /// Where the recording is on this machine, when the store is local to the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<PathBuf>,
}

/// An inclusive byte range, as in an HTTP `Range: bytes=start-end` header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    /// `None` reads to the end.
    pub end: Option<u64>,
}

impl ByteRange {
    /// Parses a single-range `bytes=` header value.
    pub fn parse_header(value: &str) -> Option<Self> {
        let (start, end) = value.trim().strip_prefix("bytes=")?.split_once('-')?;
        let start = start.trim().parse().ok()?;
        let end = match end.trim() {
            "" => None,
            end => Some(end.parse().ok()?),
        };
        if end.is_some_and(|end| end < start) {
            return None;
        }
        Some(Self { start, end })
    }
}

pub struct RecordingContent {
    pub bytes: Vec<u8>,
    /// Offset of `bytes` in the recording.
    pub start: u64,
    pub total_len: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("recording not found")]
    NotFound,
    #[error("{0} already exists")]
    AlreadyExists(String),
    #[error("expected batch {expected}")]
    OutOfOrder { expected: u64 },
    #[error("requested range is outside the recording")]
    RangeNotSatisfiable { total_len: u64 },
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Stores recordings per owner (a signed-in user's ID). Implementations must keep owners'
/// recordings apart: an ID from one owner never resolves for another.
pub trait RecordingStore: Send + Sync {
    /// Starts a recording and returns its ID.
    fn create(&self, owner: &str, request: &CreateRecording) -> Result<String, StoreError>;

    /// Appends newline-terminated asciicast event lines, already redacted.
    fn append(&self, owner: &str, id: &str, seq: u64, events: &str) -> Result<(), StoreError>;

    fn finish(&self, owner: &str, id: &str) -> Result<RecordingMetadata, StoreError>;

    /// Newest first.
    fn list(&self, owner: &str) -> Result<Vec<RecordingMetadata>, StoreError>;

    fn get(&self, owner: &str, id: &str) -> Result<RecordingMetadata, StoreError>;

    fn read(
        &self,
        owner: &str,
        id: &str,
        range: Option<ByteRange>,
    ) -> Result<RecordingContent, StoreError>;

    /// Renames the recording, which may change its ID, and returns its new metadata.
    fn rename(&self, owner: &str, id: &str, name: &str) -> Result<RecordingMetadata, StoreError>;

    fn delete(&self, owner: &str, id: &str) -> Result<(), StoreError>;
}

/// Whether `id` is a single file-name component that can't name anything outside a recordings
/// directory.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 255 && !id.starts_with('.') && !id.contains(['/', '\\', '\0'])
}
