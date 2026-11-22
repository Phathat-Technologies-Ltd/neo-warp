//! The recordings API the app talks to: the embedded server's `/recordings` routes by default,
//! or any server that implements them.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use session_recording::{CreateRecording, LocalDirStore, RecordingMetadata};

#[cfg_attr(target_family = "wasm", async_trait(?Send))]
#[cfg_attr(not(target_family = "wasm"), async_trait)]
pub trait RecordingsClient: Send + Sync {
    /// Starts a recording and returns its ID.
    async fn start(&self, request: &CreateRecording) -> anyhow::Result<String>;

    /// Appends one batch of asciicast event lines. Resending a batch that was already stored is
    /// harmless.
    async fn append(&self, id: &str, seq: u64, events: String) -> anyhow::Result<()>;

    async fn finish(&self, id: &str) -> anyhow::Result<RecordingMetadata>;

    async fn list(&self) -> anyhow::Result<Vec<RecordingMetadata>>;

    /// The recording's asciicast content.
    async fn read(&self, id: &str) -> anyhow::Result<Vec<u8>>;

    async fn rename(&self, id: &str, name: &str) -> anyhow::Result<RecordingMetadata>;

    async fn delete(&self, id: &str) -> anyhow::Result<()>;
}

/// The store behind the embedded server's `/recordings`, shared so the app can point it at the
/// configured recordings directory.
pub fn local_store() -> Arc<LocalDirStore> {
    static STORE: OnceLock<Arc<LocalDirStore>> = OnceLock::new();
    STORE
        .get_or_init(|| Arc::new(LocalDirStore::new(super::settings::default_recordings_dir())))
        .clone()
}

/// Points the local store at `dir` if it isn't already.
pub fn sync_local_store_root(dir: PathBuf) {
    let store = local_store();
    if store.root() != dir {
        store.set_root(dir);
    }
}
