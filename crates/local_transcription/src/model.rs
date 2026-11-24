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

const PARAKEET_FP16_REPO: &str = "https://huggingface.co/phathat/parakeet-tdt-0.6b-v3-fp16-coreml/resolve/eb17ca2d5fbe1c5a865a10714bd6ee3a87c6672a";

/// The fp16 conversion of Parakeet TDT 0.6B v3's encoder (CC BY 4.0) that Core ML runs on the
/// Neural Engine, from istupakov's fp32 export by `scripts/export_parakeet_fp16.py`. It shares
/// [`parakeet_tdt_0_6b_v3`]'s directory, where the engine looks for it, so removing that model
/// removes it too, and removing this one would remove both.
pub fn parakeet_fp16_encoder() -> ModelSpec {
    ModelSpec {
        id: parakeet_tdt_0_6b_v3().id,
        display_name: "Parakeet Neural Engine encoder".to_owned(),
        files: vec![ModelFile {
            name: "encoder-model.fp16.onnx".to_owned(),
            url: format!("{PARAKEET_FP16_REPO}/encoder-model.fp16.onnx"),
            size: 1_238_960_470,
            sha256: "6e2cf61d33b79a89b4322f08c7678e75b288bde4c4bb29cbbd76aea3e2b4eaa7".to_owned(),
        }],
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

/// Pipecat Smart Turn v3.2 (BSD-2-Clause), the int8 CPU build: decides from the sound of an
/// utterance whether the speaker finished their turn. Pinned to a Hugging Face revision.
pub fn smart_turn_v3_2() -> ModelSpec {
    ModelSpec {
        id: "smart-turn-v3.2-cpu".to_owned(),
        display_name: "Smart Turn v3.2".to_owned(),
        files: vec![ModelFile {
            name: "smart-turn-v3.2-cpu.onnx".to_owned(),
            url: "https://huggingface.co/pipecat-ai/smart-turn-v3/resolve/f766f81d3cfdf7737ac64aad813d91bbfd56bf93/smart-turn-v3.2-cpu.onnx"
                .to_owned(),
            size: 8_679_182,
            sha256: "2bb026316b14a660486a75b1733cd3fbab8c2fd0314dc9af7be49f8cca967e4f".to_owned(),
        }],
    }
}

/// CAM++ speaker embedding model (3D-Speaker "common advanced", Apache-2.0) trained on a large
/// Chinese-English corpus, as exported to ONNX by sherpa-onnx. Same features and size as the
/// VoxCeleb CAM++, but more robust on short, reverberant microphone speech. The release asset
/// isn't immutable, so the SHA-256 pins it.
pub fn campplus_zh_en_advanced() -> ModelSpec {
    ModelSpec {
        id: "campplus-zh-en-advanced-16k".to_owned(),
        display_name: "CAM++ speaker model".to_owned(),
        files: vec![ModelFile {
            name: "campplus_zh_en_advanced_16k.onnx".to_owned(),
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx"
                .to_owned(),
            size: 28_281_165,
            sha256: "53cab20ba8f2eb2f6e2d89ecc18e88ef3222169b61b4419cbc13666744a4574b".to_owned(),
        }],
    }
}

const CMUDICT_COMMIT: &str = "74790861f652b15e4ac49015a90074ad62a27690";

/// CMUdict (BSD-style license), which the speech output models pronounce words with.
fn cmudict_file() -> ModelFile {
    ModelFile {
        name: "cmudict.dict".to_owned(),
        url: format!(
            "https://raw.githubusercontent.com/cmusphinx/cmudict/{CMUDICT_COMMIT}/cmudict.dict"
        ),
        size: 3_618_488,
        sha256: "81917843c7f44ce2b094ac63873c2c7a4cf802040792c455ba3ca406891c3d22".to_owned(),
    }
}

const KOKORO_REPO: &str = "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/1939ad2a8e416c0acfeecc08a694d14ef25f2231";

/// Kokoro-82M v1.0 (Apache-2.0), int8 ONNX export, with four American English voices.
pub fn kokoro_82m_v1_int8() -> ModelSpec {
    let file = |name: &str, path: &str, size, sha256: &str| ModelFile {
        name: name.to_owned(),
        url: format!("{KOKORO_REPO}/{path}"),
        size,
        sha256: sha256.to_owned(),
    };
    let voice = |id: &str, sha256: &str| {
        file(
            &format!("{id}.bin"),
            &format!("voices/{id}.bin"),
            522_240,
            sha256,
        )
    };
    ModelSpec {
        id: "kokoro-82m-v1.0-int8".to_owned(),
        display_name: "Kokoro 82M".to_owned(),
        files: vec![
            file(
                "model_quantized.onnx",
                "onnx/model_quantized.onnx",
                92_361_116,
                "fbae9257e1e05ffc727e951ef9b9c98418e6d79f1c9b6b13bd59f5c9028a1478",
            ),
            file(
                "tokenizer.json",
                "tokenizer.json",
                3_497,
                "77a02c8e164413299b4b4c403b14f8e0e1c1b727db4d46a09d6327b861060a34",
            ),
            voice(
                "af_heart",
                "d583ccff3cdca2f7fae535cb998ac07e9fcb90f09737b9a41fa2734ec44a8f0b",
            ),
            voice(
                "af_bella",
                "f69d836209b78eb8c66e75e3cda491e26ea838a3674257e9d4e5703cbaf55c8b",
            ),
            voice(
                "am_michael",
                "1d1f21dd8da39c30705cd4c75d039d265e9bc4a2a93ed09bc9e1b1225eb95ba1",
            ),
            voice(
                "am_adam",
                "162b035ed91cfc48b6046982184c645f72edcdd1b82843347f605d7bf7b15716",
            ),
            cmudict_file(),
        ],
    }
}

const TINY_VOCA_REPO: &str =
    "https://huggingface.co/n4ze3m/tiny-voca/resolve/5fe06641d0afecb3c04bc9faca2cf64d9b2b1600";

/// tiny-voca (Apache-2.0), a 4.5M-parameter model with two American English voices.
pub fn tiny_voca() -> ModelSpec {
    let file = |name: &str, path: &str, size, sha256: &str| ModelFile {
        name: name.to_owned(),
        url: format!("{TINY_VOCA_REPO}/{path}"),
        size,
        sha256: sha256.to_owned(),
    };
    ModelSpec {
        id: "tiny-voca-5fe0664".to_owned(),
        display_name: "tiny-voca".to_owned(),
        files: vec![
            file(
                "model.onnx",
                "onnx/model.onnx",
                21_386_372,
                "fe6d918791a15b82e37465981d7f27a070e46b46635e5a3482fc8dff87a1ae46",
            ),
            file(
                "config.json",
                "config.json",
                2_129,
                "27bcbf322fc428581f0e959be1c141dd7751c61f4fed5eded326e779b215f69c",
            ),
            cmudict_file(),
        ],
    }
}

const POCKET_REPO: &str = "https://huggingface.co/soniqo/Pocket-TTS-100M-ONNX-INT8/resolve/68ab0441c6bba7228fd88041c8f2c909f1913d1a";

/// Kyutai Pocket TTS (CC BY 4.0), int8 ONNX export with the baked-in `alba` voice (Alba
/// MacKenna, CC BY 4.0), whose attribution ships in the bundled `LICENSE`.
pub fn pocket_tts_int8() -> ModelSpec {
    let file = |name: &str, size, sha256: &str| ModelFile {
        name: name.to_owned(),
        url: format!("{POCKET_REPO}/{name}"),
        size,
        sha256: sha256.to_owned(),
    };
    ModelSpec {
        id: "pocket-tts-100m-int8-68ab044".to_owned(),
        display_name: "Pocket TTS".to_owned(),
        files: vec![
            file(
                "lm_main.int8.onnx",
                76_341_079,
                "bfc0c7e7e3d72864fa3bb2ee499f62f21ddc1474b885f5f3ca570f8be73e787e",
            ),
            file(
                "lm_flow.int8.onnx",
                9_962_530,
                "8d627d235c44a597da908e1085ebe241cbbe358964c502c5a5063d18851a5529",
            ),
            file(
                "decoder.int8.onnx",
                22_695_710,
                "ed8d050fc5da275cfca88224b7d2cc29fde7c23e133618f346e98ac505b3d862",
            ),
            file(
                "encoder.onnx",
                512_407,
                "2194513df47271ece9f8d2d571facd57b24d96a1d912fdc13bf0e10c69318e85",
            ),
            file(
                "text_conditioner.onnx",
                16_388_498,
                "5217b8474621af91127cfef891714337ae8cba106710ce04a426ec4bd56bbd1e",
            ),
            file(
                "vocab.json",
                69_479,
                "a2673c232cf49dd6eb1ad850e7c7682f6443c2ab64040d1150e9d8f2a7e3587b",
            ),
            file(
                "token_scores.json",
                123_617,
                "3baa6ef7d57bac245271e33f96161f3ac60038d753f13f3fdbe24a7d2422ad6b",
            ),
            file(
                "LICENSE",
                18_657,
                "9ba9550ad48438d0836ddab3da480b3b69ffa0aac7b7878b5a0039e7ab429411",
            ),
        ],
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
