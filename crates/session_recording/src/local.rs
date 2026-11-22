//! A [`RecordingStore`] that keeps each owner's recordings as `.cast` files in a subdirectory of
//! a root directory.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError, RwLock};

use chrono::{Local, TimeZone as _};
use sha2::{Digest as _, Sha256};

use crate::asciicast::{self, CastWriter, FILE_EXTENSION};
use crate::naming::{self, RecordingInfo};
use crate::store::{
    ByteRange, CreateRecording, RecordingContent, RecordingMetadata, RecordingStore, StoreError,
    is_valid_id,
};

pub struct LocalDirStore {
    root: RwLock<PathBuf>,
    /// The next expected batch of each recording still being written, by `(owner, id)`.
    next_seq: Mutex<HashMap<(String, String), u64>>,
}

impl LocalDirStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root: RwLock::new(root),
            next_seq: Mutex::default(),
        }
    }

    pub fn root(&self) -> PathBuf {
        self.root
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Moves the store to `root` for later requests; existing files are not moved.
    pub fn set_root(&self, root: PathBuf) {
        *self.root.write().unwrap_or_else(PoisonError::into_inner) = root;
    }

    /// The directory holding `owner`'s recordings. Owner IDs are hashed so any ID maps to a safe,
    /// distinct directory name.
    pub fn owner_dir(&self, owner: &str) -> PathBuf {
        let digest = Sha256::digest(owner.as_bytes());
        self.root()
            .join(format!("{OWNER_DIR_PREFIX}{}", &hex::encode(digest)[..16]))
    }

    /// Moves recordings saved directly in the root (the layout before per-user folders) into
    /// `owner`'s folder, but only while `owner` is the only user of the store; with several users
    /// there's no telling whose they are, so they are left in place.
    fn migrate_flat_layout(&self, owner: &str) {
        let root = self.root();
        let Ok(entries) = fs::read_dir(&root) else {
            return;
        };
        let owner_dir = self.owner_dir(owner);
        let mut flat = Vec::new();
        let mut has_other_owner = false;
        for path in entries.filter_map(Result::ok).map(|entry| entry.path()) {
            let is_owner_dir = path.is_dir()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(OWNER_DIR_PREFIX));
            if is_owner_dir {
                has_other_owner |= path != owner_dir;
            } else if path.is_file() && path.extension().is_some_and(|ext| ext == FILE_EXTENSION) {
                flat.push(path);
            }
        }
        if flat.is_empty() {
            return;
        }
        if has_other_owner {
            log::info!(
                "Leaving {} recordings in {} because several users share it",
                flat.len(),
                root.display()
            );
            return;
        }
        if let Err(error) = fs::create_dir_all(&owner_dir) {
            log::warn!("Couldn't create {}: {error}", owner_dir.display());
            return;
        }
        for path in flat {
            let Some(name) = path.file_name() else {
                continue;
            };
            let target = owner_dir.join(name);
            if target.exists() {
                log::warn!("Not moving {}: {} exists", path.display(), target.display());
                continue;
            }
            if let Err(error) = fs::rename(&path, &target) {
                log::warn!("Couldn't move {}: {error}", path.display());
            }
        }
    }

    fn is_in_progress(&self, owner: &str, id: &str) -> bool {
        self.seqs().contains_key(&key(owner, id))
    }

    fn with_progress(&self, owner: &str, mut metadata: RecordingMetadata) -> RecordingMetadata {
        metadata.in_progress = self.is_in_progress(owner, &metadata.id);
        metadata
    }

    fn path(&self, owner: &str, id: &str) -> Result<PathBuf, StoreError> {
        if !is_valid_id(id) {
            return Err(StoreError::NotFound);
        }
        Ok(self.owner_dir(owner).join(format!("{id}.{FILE_EXTENSION}")))
    }

    fn existing_path(&self, owner: &str, id: &str) -> Result<PathBuf, StoreError> {
        let path = self.path(owner, id)?;
        if path.is_file() {
            Ok(path)
        } else {
            Err(StoreError::NotFound)
        }
    }

    fn seqs(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), u64>> {
        self.next_seq.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn key(owner: &str, id: &str) -> (String, String) {
    (owner.to_owned(), id.to_owned())
}

fn metadata(info: RecordingInfo) -> RecordingMetadata {
    RecordingMetadata {
        id: file_id(&info.path),
        title: info.title,
        started_at: info.started_at.map(|time| time.timestamp()),
        duration: info.duration,
        size_bytes: info.size_bytes,
        local_path: Some(info.path),
        in_progress: false,
    }
}

const OWNER_DIR_PREFIX: &str = "user-";

fn file_id(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn read_metadata(path: &Path) -> Result<RecordingMetadata, StoreError> {
    naming::read_info(path)
        .map(metadata)
        .map_err(|error| match error.downcast::<io::Error>() {
            Ok(error) => StoreError::Io(error),
            Err(error) => StoreError::Invalid(format!("{error:#}")),
        })
}

fn not_found_as_store_error(error: io::Error) -> StoreError {
    if error.kind() == io::ErrorKind::NotFound {
        StoreError::NotFound
    } else {
        StoreError::Io(error)
    }
}

impl RecordingStore for LocalDirStore {
    fn create(&self, owner: &str, request: &CreateRecording) -> Result<String, StoreError> {
        if request.header.version != 2 {
            return Err(StoreError::Invalid(format!(
                "unsupported asciicast version {}",
                request.header.version
            )));
        }
        self.migrate_flat_layout(owner);
        let dir = self.owner_dir(owner);
        fs::create_dir_all(&dir)?;
        let started_at = request
            .header
            .timestamp
            .and_then(|secs| Local.timestamp_opt(secs, 0).single())
            .unwrap_or_else(Local::now);
        let path = naming::new_recording_path(&dir, started_at, &request.name);
        let file = File::create_new(&path)?;
        CastWriter::new(BufWriter::new(file), &request.header)?.into_inner()?;
        let id = file_id(&path);
        self.seqs().insert(key(owner, &id), 0);
        Ok(id)
    }

    fn append(&self, owner: &str, id: &str, seq: u64, events: &str) -> Result<(), StoreError> {
        let path = self.existing_path(owner, id)?;
        // Held across the write so concurrent batches of one recording can't interleave.
        let mut seqs = self.seqs();
        let key = key(owner, id);
        // A recording missing from the map was started before this store was created (for
        // example, before a server restart), so its next batch is whatever arrives.
        let expected = seqs.get(&key).copied().unwrap_or(seq);
        if seq < expected {
            return Ok(());
        }
        if seq > expected {
            return Err(StoreError::OutOfOrder { expected });
        }
        asciicast::validate_event_lines(events)
            .map_err(|error| StoreError::Invalid(format!("{error:#}")))?;
        if !events.is_empty() {
            let mut file = OpenOptions::new().append(true).open(&path)?;
            file.write_all(events.as_bytes())?;
            if !events.ends_with('\n') {
                file.write_all(b"\n")?;
            }
        }
        seqs.insert(key, seq + 1);
        Ok(())
    }

    fn finish(&self, owner: &str, id: &str) -> Result<RecordingMetadata, StoreError> {
        let path = self.existing_path(owner, id)?;
        self.seqs().remove(&key(owner, id));
        read_metadata(&path)
    }

    fn list(&self, owner: &str) -> Result<Vec<RecordingMetadata>, StoreError> {
        self.migrate_flat_layout(owner);
        Ok(naming::list_recordings(&self.owner_dir(owner))?
            .into_iter()
            .map(|info| self.with_progress(owner, metadata(info)))
            .filter(|recording| is_valid_id(&recording.id))
            .collect())
    }

    fn get(&self, owner: &str, id: &str) -> Result<RecordingMetadata, StoreError> {
        Ok(self.with_progress(owner, read_metadata(&self.existing_path(owner, id)?)?))
    }

    fn read(
        &self,
        owner: &str,
        id: &str,
        range: Option<ByteRange>,
    ) -> Result<RecordingContent, StoreError> {
        let path = self.existing_path(owner, id)?;
        let mut file = File::open(&path).map_err(not_found_as_store_error)?;
        let total_len = file.metadata()?.len();
        let range = range.unwrap_or(ByteRange {
            start: 0,
            end: None,
        });
        if range.start > total_len || (range.start == total_len && range.start > 0) {
            return Err(StoreError::RangeNotSatisfiable { total_len });
        }
        let end = range
            .end
            .map_or(total_len, |end| end.saturating_add(1).min(total_len));
        let mut bytes = vec![0; usize::try_from(end - range.start).unwrap_or(usize::MAX)];
        file.seek(SeekFrom::Start(range.start))?;
        file.read_exact(&mut bytes)?;
        Ok(RecordingContent {
            bytes,
            start: range.start,
            total_len,
        })
    }

    fn rename(&self, owner: &str, id: &str, name: &str) -> Result<RecordingMetadata, StoreError> {
        let path = self.existing_path(owner, id)?;
        if self.is_in_progress(owner, id) {
            return Err(StoreError::InProgress);
        }
        let target = naming::rename_recording(&path, name).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                StoreError::AlreadyExists(name.trim().to_owned())
            } else {
                not_found_as_store_error(error)
            }
        })?;
        read_metadata(&target)
    }

    fn delete(&self, owner: &str, id: &str) -> Result<(), StoreError> {
        let path = self.path(owner, id)?;
        fs::remove_file(path).map_err(not_found_as_store_error)?;
        self.seqs().remove(&key(owner, id));
        Ok(())
    }
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
