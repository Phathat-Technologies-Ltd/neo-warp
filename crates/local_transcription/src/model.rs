use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use reqwest::{StatusCode, header};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// One file of a model, pinned by size and SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// A downloadable model: the files an engine loads from one directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    /// Directory name under the store root; change it when the files change.
    pub id: String,
    pub display_name: String,
    pub files: Vec<ModelFile>,
}

impl ModelSpec {
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }
}

const PARAKEET_REPO: &str =
    "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main";

/// NVIDIA Parakeet TDT 0.6B v3, int8 ONNX export (25 European languages).
pub fn parakeet_tdt_0_6b_v3() -> ModelSpec {
    let file = |name: &str, size, sha256: &str| ModelFile {
        name: name.to_owned(),
        url: format!("{PARAKEET_REPO}/{name}"),
        size,
        sha256: sha256.to_owned(),
    };
    ModelSpec {
        id: "parakeet-tdt-0.6b-v3-int8".to_owned(),
        display_name: "Parakeet TDT 0.6B v3".to_owned(),
        files: vec![
            file(
                "encoder-model.int8.onnx",
                652_183_999,
                "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09",
            ),
            file(
                "decoder_joint-model.int8.onnx",
                18_202_004,
                "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70",
            ),
            file(
                "vocab.txt",
                93_939,
                "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
            ),
        ],
    }
}

/// Silero VAD v5 (MIT), from the `v5.1.2` release of `snakers4/silero-vad`.
pub fn silero_vad_v5() -> ModelSpec {
    ModelSpec {
        id: "silero-vad-v5.1.2".to_owned(),
        display_name: "Silero VAD v5".to_owned(),
        files: vec![ModelFile {
            name: "silero_vad.onnx".to_owned(),
            url: "https://github.com/snakers4/silero-vad/raw/v5.1.2/src/silero_vad/data/silero_vad.onnx"
                .to_owned(),
            size: 2_327_524,
            sha256: "2623a2953f6ff3d2c1e61740c6cdb7168133479b267dfef114a4a3cc5bdd788f".to_owned(),
        }],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelStatus {
    NotInstalled,
    /// Some files, or partial files, from an interrupted download are on disk.
    Partial {
        downloaded: u64,
    },
    Installed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: u64,
}

impl DownloadProgress {
    /// Whole percent downloaded, or `None` before the total is known.
    pub fn percent(&self) -> Option<u64> {
        (self.total > 0).then(|| self.downloaded.min(self.total) * 100 / self.total)
    }
}

/// Limits how often download progress reaches the UI: a report goes through when the whole
/// percent changes, when `interval` has passed since the last one, or when the download completes.
#[derive(Debug, Clone)]
pub struct ProgressThrottle {
    interval: Duration,
    last: Option<(Option<u64>, Instant)>,
}

impl ProgressThrottle {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    pub fn should_report(&mut self, progress: DownloadProgress, now: Instant) -> bool {
        let percent = progress.percent();
        let report = match self.last {
            None => true,
            Some((last_percent, at)) => {
                last_percent != percent
                    || progress.downloaded >= progress.total
                    || now.saturating_duration_since(at) >= self.interval
            }
        };
        if report {
            self.last = Some((percent, now));
        }
        report
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("download cancelled")]
    Cancelled,
    #[error("couldn't download {file}: {source}")]
    Http {
        file: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("couldn't download {file}: server answered {status}")]
    Status { file: String, status: StatusCode },
    #[error("{file} failed its checksum; it was deleted, so try again")]
    Checksum { file: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Keeps one model's files in `<root>/<spec id>`. A download writes `<file>.part`, resumes it
/// with an HTTP range request, verifies the SHA-256, then renames it; a marker file records that
/// every file verified so later launches don't rehash hundreds of megabytes.
#[derive(Debug, Clone)]
pub struct ModelStore {
    dir: PathBuf,
    spec: ModelSpec,
}

const VERIFIED_MARKER: &str = ".verified";

impl ModelStore {
    pub fn new(root: impl AsRef<Path>, spec: ModelSpec) -> Self {
        Self {
            dir: root.as_ref().join(&spec.id),
            spec,
        }
    }

    pub fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    /// The directory an engine loads the model from.
    pub fn model_dir(&self) -> &Path {
        &self.dir
    }

    pub fn status(&self) -> ModelStatus {
        let all_present = self.spec.files.iter().all(|file| {
            std::fs::metadata(self.dir.join(&file.name)).is_ok_and(|meta| meta.len() == file.size)
        });
        if all_present && self.dir.join(VERIFIED_MARKER).is_file() {
            return ModelStatus::Installed;
        }
        let downloaded: u64 = self
            .spec
            .files
            .iter()
            .map(|file| {
                let len = |path: PathBuf| std::fs::metadata(path).map_or(0, |meta| meta.len());
                len(self.dir.join(&file.name)).max(len(self.part_path(file)))
            })
            .sum();
        if downloaded == 0 {
            ModelStatus::NotInstalled
        } else {
            ModelStatus::Partial { downloaded }
        }
    }

    pub fn is_installed(&self) -> bool {
        self.status() == ModelStatus::Installed
    }

    /// Deletes the model, including partial downloads.
    pub fn remove(&self) -> io::Result<()> {
        match std::fs::remove_dir_all(&self.dir) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }

    /// Downloads whatever is missing, resuming partial files, and verifies every file. Checks
    /// `cancel` between chunks; a cancelled download keeps its partial files for resuming.
    pub async fn download(
        &self,
        client: &reqwest::Client,
        cancel: &AtomicBool,
        mut on_progress: impl FnMut(DownloadProgress),
    ) -> Result<(), DownloadError> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let total = self.spec.total_size();
        let mut done_before = 0;
        for file in &self.spec.files {
            let final_path = self.dir.join(&file.name);
            let have_final = tokio::fs::metadata(&final_path)
                .await
                .is_ok_and(|meta| meta.len() == file.size);
            if !have_final {
                self.download_file(client, file, cancel, |file_downloaded| {
                    on_progress(DownloadProgress {
                        downloaded: done_before + file_downloaded,
                        total,
                    })
                })
                .await?;
            }
            done_before += file.size;
            on_progress(DownloadProgress {
                downloaded: done_before,
                total,
            });
        }
        tokio::fs::write(self.dir.join(VERIFIED_MARKER), b"").await?;
        Ok(())
    }

    fn part_path(&self, file: &ModelFile) -> PathBuf {
        self.dir.join(format!("{}.part", file.name))
    }

    async fn download_file(
        &self,
        client: &reqwest::Client,
        file: &ModelFile,
        cancel: &AtomicBool,
        mut on_progress: impl FnMut(u64),
    ) -> Result<(), DownloadError> {
        let part_path = self.part_path(file);
        let mut offset = tokio::fs::metadata(&part_path)
            .await
            .map_or(0, |meta| meta.len());
        if offset > file.size {
            tokio::fs::remove_file(&part_path).await?;
            offset = 0;
        }
        if offset < file.size {
            let http = |source| DownloadError::Http {
                file: file.name.clone(),
                source,
            };
            let mut request = client
                .get(&file.url)
                // Byte ranges must address the stored file, not a compressed rendition of it.
                .header(header::ACCEPT_ENCODING, "identity");
            if offset > 0 {
                request = request.header(header::RANGE, format!("bytes={offset}-"));
            }
            let response = request.send().await.map_err(http)?;
            let append = match response.status() {
                StatusCode::PARTIAL_CONTENT => true,
                status if status.is_success() => false,
                status => {
                    return Err(DownloadError::Status {
                        file: file.name.clone(),
                        status,
                    });
                }
            };
            if !append {
                offset = 0;
            }
            let mut out = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(append)
                .truncate(!append)
                .open(&part_path)
                .await?;
            on_progress(offset);
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                if cancel.load(Ordering::Relaxed) {
                    out.flush().await?;
                    return Err(DownloadError::Cancelled);
                }
                let chunk = chunk.map_err(http)?;
                out.write_all(&chunk).await?;
                offset += chunk.len() as u64;
                on_progress(offset);
            }
            out.flush().await?;
        }
        if sha256_file(&part_path).await? != file.sha256 {
            tokio::fs::remove_file(&part_path).await?;
            return Err(DownloadError::Checksum {
                file: file.name.clone(),
            });
        }
        tokio::fs::rename(&part_path, self.dir.join(&file.name)).await?;
        Ok(())
    }
}

/// Models a feature needs together, downloaded after one consent with one progress total.
#[derive(Debug, Clone)]
pub struct ModelBundle {
    stores: Vec<ModelStore>,
}

impl ModelBundle {
    pub fn new(stores: Vec<ModelStore>) -> Self {
        Self { stores }
    }

    pub fn stores(&self) -> &[ModelStore] {
        &self.stores
    }

    pub fn total_size(&self) -> u64 {
        self.stores
            .iter()
            .map(|store| store.spec().total_size())
            .sum()
    }

    /// The models' names, e.g. "Parakeet TDT 0.6B v3 and Silero VAD v5".
    pub fn display_name(&self) -> String {
        let names: Vec<&str> = self
            .stores
            .iter()
            .map(|store| store.spec().display_name.as_str())
            .collect();
        match names.split_last() {
            None => String::new(),
            Some((last, [])) => (*last).to_owned(),
            Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        }
    }

    pub fn status(&self) -> ModelStatus {
        let statuses: Vec<ModelStatus> = self.stores.iter().map(ModelStore::status).collect();
        if statuses
            .iter()
            .all(|status| *status == ModelStatus::Installed)
        {
            return ModelStatus::Installed;
        }
        let downloaded: u64 = self
            .stores
            .iter()
            .zip(&statuses)
            .map(|(store, status)| match status {
                ModelStatus::NotInstalled => 0,
                ModelStatus::Partial { downloaded } => *downloaded,
                ModelStatus::Installed => store.spec().total_size(),
            })
            .sum();
        if downloaded == 0 {
            ModelStatus::NotInstalled
        } else {
            ModelStatus::Partial { downloaded }
        }
    }

    pub fn is_installed(&self) -> bool {
        self.stores.iter().all(ModelStore::is_installed)
    }

    pub fn remove(&self) -> io::Result<()> {
        self.stores.iter().try_for_each(ModelStore::remove)
    }

    /// Downloads every model that isn't installed, reporting progress against the bundle's total.
    pub async fn download(
        &self,
        client: &reqwest::Client,
        cancel: &AtomicBool,
        mut on_progress: impl FnMut(DownloadProgress),
    ) -> Result<(), DownloadError> {
        let total = self.total_size();
        let mut done_before = 0;
        for store in &self.stores {
            if !store.is_installed() {
                store
                    .download(client, cancel, |progress| {
                        on_progress(DownloadProgress {
                            downloaded: done_before + progress.downloaded,
                            total,
                        })
                    })
                    .await?;
            }
            done_before += store.spec().total_size();
            on_progress(DownloadProgress {
                downloaded: done_before,
                total,
            });
        }
        Ok(())
    }
}

async fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
