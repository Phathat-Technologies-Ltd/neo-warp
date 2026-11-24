use std::path::Path;

use parakeet_rs::ExecutionConfig;
use serde::{Deserialize, Serialize};

use super::TdtModel;
use crate::TranscribeError;

pub(crate) use core_ml::Accelerator;

/// ONNX Runtime intra-op threads for Parakeet on the CPU. More threads barely shorten a decode but
/// keep more cores busy while the user talks.
pub const DEFAULT_INTRA_THREADS: usize = 3;

/// The optional fp16 export of Parakeet's encoder that Core ML runs. Core ML can't run the int8
/// encoder: its quantized ops stay on the CPU, split into hundreds of partitions.
pub const FP16_ENCODER_FILE: &str = "encoder-model.fp16.onnx";

/// Where Parakeet's encoder runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Acceleration {
    /// Core ML (Neural Engine / GPU) where it's available and matches the CPU, else the CPU.
    #[default]
    Auto,
    Cpu,
}

/// Where the encoder of a loaded model runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderDevice {
    Cpu,
    CoreMl,
}

impl TdtModel {
    /// Loads the model in `dir` with the process's ONNX Runtime environment, on the CPU. With
    /// [`Acceleration::Auto`] and the fp16 encoder in `dir`, the encoder also loads on Core ML in
    /// the background and takes over once it encodes like the CPU encoder, only faster. The
    /// decoder and joint network always run on the CPU, which is faster for their tiny per-step
    /// calls.
    pub fn load_accelerated(
        dir: &Path,
        acceleration: Acceleration,
        intra_threads: Option<usize>,
    ) -> Result<Self, TranscribeError> {
        let config = ExecutionConfig::new()
            .with_environment(crate::ort_env::environment()?)
            .with_intra_threads(intra_threads.unwrap_or(DEFAULT_INTRA_THREADS));
        let mut model = Self::load(dir, &config)?;
        if acceleration == Acceleration::Auto {
            model.accelerator = core_ml::start(&mut model, dir);
        }
        Ok(model)
    }

    /// Where the encoder runs now. Core ML takes over at the first encode after it's ready.
    pub fn encoder_device(&self) -> EncoderDevice {
        self.accelerator.device()
    }

    /// Blocks until a background Core ML load finishes, returning where the encoder runs then.
    pub fn wait_for_accelerator(&mut self) -> EncoderDevice {
        self.accelerator.wait();
        self.accelerator.device()
    }
}

#[cfg(all(feature = "coreml", target_os = "macos"))]
mod core_ml {
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{Receiver, TryRecvError, channel};
    use std::time::Duration;

    use instant::Instant;

    use parakeet_rs::ort::ep::coreml::{ComputeUnits, CoreML, ModelFormat};
    use parakeet_rs::ort::inputs;
    use parakeet_rs::ort::session::Session;
    use parakeet_rs::ort::session::builder::GraphOptimizationLevel;
    use parakeet_rs::ort::value::Tensor;

    use super::{EncoderDevice, FP16_ENCODER_FILE, TdtModel};
    use crate::TranscribeError;
    use crate::tdt::{N_MELS, cosine_similarity, encoder_output, engine_error};

    /// Input lengths in feature frames (10 ms each) that Core ML compiles the encoder for: the
    /// Neural Engine runs only static shapes, and a shorter input runs faster padded to the
    /// smallest bucket that fits. 30 s covers the longest utterance the stream hands over. Each
    /// bucket compiles in about 50 s on first use; Core ML placed these sizes on the Neural Engine
    /// on an M4 Max but put 800 and 1000 frames on the GPU.
    const BUCKET_FRAMES: [usize; 3] = [500, 1500, 3000];
    /// Core ML must reproduce the CPU encoder's output on the probe this closely (cosine
    /// similarity). Low because the CPU encoder is int8: on speech, fp16 on the Neural Engine
    /// matches the fp32 encoder at 0.98-0.9998 while int8 matches it at only 0.92-0.95, and the
    /// synthetic probe lands at 0.75, so this only rules out broken output.
    const MIN_SIMILARITY: f32 = 0.6;
    /// Holds "cpu" once Core ML has proven slower than or different from the CPU, so later loads
    /// skip compiling it.
    const VERDICT_FILE: &str = "verdict";
    /// Counts Core ML loads that haven't finished, so one that keeps crashing (or never finishes)
    /// sends later launches to the CPU.
    const LOADING_MARKER: &str = "loading";
    const MAX_UNFINISHED_LOADS: u32 = 3;

    /// The encoder's Core ML sessions, one per input length.
    pub struct Buckets(Vec<(usize, Session)>);

    /// The encoder on Core ML: loading in the background, or ready to replace the CPU encoder.
    pub enum Accelerator {
        Off,
        Loading(Receiver<Result<Buckets, TranscribeError>>),
        Ready(Buckets),
    }

    impl Accelerator {
        pub(in crate::tdt) fn off() -> Self {
            Self::Off
        }

        pub(in crate::tdt) fn device(&self) -> EncoderDevice {
            match self {
                Self::Ready(_) => EncoderDevice::CoreMl,
                Self::Off | Self::Loading(_) => EncoderDevice::Cpu,
            }
        }

        pub(in crate::tdt) fn wait(&mut self) {
            if let Self::Loading(ready) = self {
                *self = match ready.recv() {
                    Ok(result) => Self::from_load(result),
                    Err(_) => Self::Off,
                };
            }
        }

        fn poll(&mut self) {
            if let Self::Loading(ready) = self {
                match ready.try_recv() {
                    Ok(result) => *self = Self::from_load(result),
                    Err(TryRecvError::Disconnected) => *self = Self::Off,
                    Err(TryRecvError::Empty) => {}
                }
            }
        }

        fn from_load(result: Result<Buckets, TranscribeError>) -> Self {
            match result {
                Ok(buckets) => Self::Ready(buckets),
                Err(error) => {
                    log::warn!("Parakeet: encoder stays on the CPU: {error}");
                    Self::Off
                }
            }
        }

        /// Encodes `features` on Core ML, or returns `None` when Core ML isn't ready or no bucket
        /// fits them.
        pub(in crate::tdt) fn encode(
            &mut self,
            features: &[f32],
            frames: usize,
        ) -> Result<Option<(Vec<f32>, usize, usize)>, TranscribeError> {
            self.poll();
            let Self::Ready(buckets) = self else {
                return Ok(None);
            };
            buckets.encode(features, frames)
        }
    }

    impl Buckets {
        fn encode(
            &mut self,
            features: &[f32],
            frames: usize,
        ) -> Result<Option<(Vec<f32>, usize, usize)>, TranscribeError> {
            let Some((bucket, session)) = self.0.iter_mut().find(|(len, _)| *len >= frames) else {
                return Ok(None);
            };
            let bucket = *bucket;
            let mut padded = vec![0.0f32; N_MELS * bucket];
            for (row, mel) in padded
                .chunks_exact_mut(bucket)
                .zip(features.chunks_exact(frames))
            {
                row[..frames].copy_from_slice(mel);
            }
            let signal =
                Tensor::from_array(([1usize, N_MELS, bucket], padded)).map_err(engine_error)?;
            let length =
                Tensor::from_array(([1usize], vec![frames as i64])).map_err(engine_error)?;
            let outputs = session
                .run(inputs!["audio_signal" => signal, "length" => length])
                .map_err(engine_error)?;
            encoder_output(&outputs).map(Some)
        }
    }

    /// Core ML's compiled-model cache for the model in `dir`, beside the model rather than in it
    /// so the model store's files stay as downloaded.
    fn cache_dir(dir: &Path) -> PathBuf {
        let name = dir.file_name().map_or_else(
            || "model".into(),
            |name| name.to_string_lossy().into_owned(),
        );
        dir.with_file_name(format!("{name}.coreml-cache"))
    }

    /// Starts loading the fp16 encoder in `dir` on Core ML in the background, checked against
    /// `model`'s CPU encoder, unless it's missing or an earlier load ruled Core ML out.
    pub(super) fn start(model: &mut TdtModel, dir: &Path) -> Accelerator {
        let encoder = dir.join(FP16_ENCODER_FILE);
        if !encoder.exists() {
            return Accelerator::Off;
        }
        let cache = cache_dir(dir);
        if std::fs::read_to_string(cache.join(VERDICT_FILE)).is_ok_and(|verdict| verdict == "cpu") {
            log::info!("Parakeet: encoder on the CPU, which beat Core ML on this Mac");
            return Accelerator::Off;
        }
        let marker = cache.join(LOADING_MARKER);
        let unfinished = std::fs::read_to_string(&marker)
            .ok()
            .and_then(|count| count.trim().parse::<u32>().ok())
            .unwrap_or(0);
        if unfinished >= MAX_UNFINISHED_LOADS {
            log::warn!(
                "Parakeet: {unfinished} Core ML loads didn't finish, keeping the encoder on the \
                 CPU (delete {} to retry)",
                marker.display()
            );
            return Accelerator::Off;
        }
        if let Err(error) = std::fs::create_dir_all(&cache)
            .and_then(|()| std::fs::write(&marker, (unfinished + 1).to_string()))
        {
            log::warn!("Parakeet: no Core ML cache, keeping the encoder on the CPU: {error}");
            return Accelerator::Off;
        }
        let probe = match Probe::new(model) {
            Ok(probe) => probe,
            Err(error) => {
                let _ = std::fs::remove_file(&marker);
                log::warn!("Parakeet: encoder stays on the CPU: {error}");
                return Accelerator::Off;
            }
        };
        let (sender, receiver) = channel();
        let spawned = std::thread::Builder::new()
            .name("parakeet-coreml".to_owned())
            .spawn(move || {
                super::lower_thread_priority();
                let started = Instant::now();
                let result = std::panic::catch_unwind(|| load(&encoder, &cache, &probe))
                    .unwrap_or_else(|_| Err(TranscribeError::Engine("Core ML panicked".into())));
                let _ = std::fs::remove_file(&marker);
                let result = match result {
                    Ok(Verdict::CoreMl(buckets)) => {
                        log::info!(
                            "Parakeet: encoder on Core ML (loaded and checked in {:?})",
                            started.elapsed()
                        );
                        Ok(buckets)
                    }
                    Ok(Verdict::Cpu(reason)) => {
                        let _ = std::fs::write(cache.join(VERDICT_FILE), "cpu");
                        Err(TranscribeError::Engine(reason))
                    }
                    Err(error) => Err(error),
                };
                let _ = sender.send(result);
            });
        match spawned {
            Ok(_) => Accelerator::Loading(receiver),
            Err(error) => {
                log::warn!("Parakeet: encoder stays on the CPU: {error}");
                Accelerator::Off
            }
        }
    }

    /// The CPU encoder's output and time for a probe input, for Core ML to match and beat.
    struct Probe {
        features: Vec<f32>,
        frames: usize,
        expected: Vec<f32>,
        cpu_time: Duration,
    }

    impl Probe {
        fn new(model: &mut TdtModel) -> Result<Self, TranscribeError> {
            let (features, frames) = model.features(&probe_audio())?;
            let (expected, _, _) = model.run_cpu_encoder(features.clone(), frames)?;
            let started = Instant::now();
            model.run_cpu_encoder(features.clone(), frames)?;
            Ok(Self {
                features,
                frames,
                expected,
                cpu_time: started.elapsed(),
            })
        }
    }

    enum Verdict {
        CoreMl(Buckets),
        Cpu(String),
    }

    fn load(encoder: &Path, cache: &Path, probe: &Probe) -> Result<Verdict, TranscribeError> {
        let mut buckets = Vec::with_capacity(BUCKET_FRAMES.len());
        for frames in BUCKET_FRAMES {
            let session = load_bucket(encoder, &cache.join(format!("fp16-{frames}")), frames)?;
            buckets.push((frames, session));
        }
        let mut buckets = Buckets(buckets);
        // The first run on each bucket specializes it for the device, which takes a while.
        for frames in BUCKET_FRAMES {
            buckets.encode(&vec![0.0; N_MELS * frames], frames)?;
        }
        let Some((actual, _, _)) = buckets.encode(&probe.features, probe.frames)? else {
            return Err(TranscribeError::Engine("no bucket fits the probe".into()));
        };
        let similarity = cosine_similarity(&probe.expected, &actual);
        if actual.len() != probe.expected.len() || similarity < MIN_SIMILARITY {
            return Ok(Verdict::Cpu(format!(
                "Core ML output differs from the CPU (similarity {similarity:.4})"
            )));
        }
        let started = Instant::now();
        buckets.encode(&probe.features, probe.frames)?;
        let core_ml = started.elapsed();
        if core_ml >= probe.cpu_time {
            return Ok(Verdict::Cpu(format!(
                "the CPU beat Core ML ({:?} vs {core_ml:?})",
                probe.cpu_time
            )));
        }
        Ok(Verdict::CoreMl(buckets))
    }

    fn load_bucket(
        encoder: &Path,
        cache: &Path,
        frames: usize,
    ) -> Result<Session, TranscribeError> {
        let core_ml = CoreML::default()
            .with_model_format(ModelFormat::MLProgram)
            .with_compute_units(ComputeUnits::All)
            .with_model_cache_dir(cache.to_string_lossy())
            .build()
            .error_on_failure();
        Session::builder(&crate::ort_env::environment()?)
            .map_err(engine_error)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(engine_error)?
            .with_intra_threads(1)
            .map_err(engine_error)?
            .with_dimension_override("audio_signal_dynamic_axes_1", 1)
            .map_err(engine_error)?
            .with_dimension_override("length_dynamic_axes_1", 1)
            .map_err(engine_error)?
            .with_dimension_override("audio_signal_dynamic_axes_2", frames as i64)
            .map_err(engine_error)?
            .with_execution_providers([core_ml])
            .map_err(engine_error)?
            .commit_from_file(encoder)
            .map_err(engine_error)
    }

    /// Two seconds of a voiced, gliding tone: deterministic and speech-like enough to exercise
    /// the whole encoder.
    fn probe_audio() -> Vec<f32> {
        (0..32_000)
            .map(|i| {
                let t = i as f32 / 16_000.0;
                let pitch = 120.0 + 60.0 * (t * 1.7).sin();
                (1..=8)
                    .map(|harmonic| {
                        let h = harmonic as f32;
                        (std::f32::consts::TAU * pitch * h * t).sin() / h
                    })
                    .sum::<f32>()
                    * 0.2
                    * (0.5 + 0.5 * (t * 9.0).sin())
            })
            .collect()
    }
}

#[cfg(not(all(feature = "coreml", target_os = "macos")))]
mod core_ml {
    use std::path::Path;

    use super::{EncoderDevice, TdtModel};
    use crate::TranscribeError;

    /// Core ML isn't available in this build.
    pub struct Accelerator;

    impl Accelerator {
        pub(in crate::tdt) fn off() -> Self {
            Self
        }

        pub(in crate::tdt) fn device(&self) -> EncoderDevice {
            EncoderDevice::Cpu
        }

        pub(in crate::tdt) fn wait(&mut self) {}

        pub(in crate::tdt) fn encode(
            &mut self,
            _features: &[f32],
            _frames: usize,
        ) -> Result<Option<(Vec<f32>, usize, usize)>, TranscribeError> {
            Ok(None)
        }
    }

    pub(super) fn start(_model: &mut TdtModel, _dir: &Path) -> Accelerator {
        Accelerator
    }
}

/// Runs the calling thread, and the Core ML compile work it starts, at background QoS: the
/// one-time compile takes minutes and dictation keeps running on the CPU meanwhile, so it must not
/// compete with the user's work.
fn lower_thread_priority() {
    #[cfg(target_os = "macos")]
    // SAFETY: only changes the calling thread's QoS class; no pointers are involved.
    unsafe {
        let status =
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
        if status != 0 {
            log::warn!("Parakeet: couldn't lower the Core ML compile priority ({status})");
        }
    }
}
