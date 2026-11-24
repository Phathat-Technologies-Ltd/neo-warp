use std::path::Path;

use parakeet_rs::ExecutionConfig;
use serde::{Deserialize, Serialize};

use super::TdtModel;
use crate::TranscribeError;

pub(crate) use core_ml::Accelerator;
use route::Route;

/// ONNX Runtime intra-op threads for Parakeet on the CPU. More threads barely shorten a decode but
/// keep more cores busy while the user talks.
pub const DEFAULT_INTRA_THREADS: usize = 3;

/// The optional fp16 export of Parakeet's encoder that Core ML runs through ONNX Runtime. Core ML
/// can't run the int8 encoder: its quantized ops stay on the CPU, split into hundreds of
/// partitions.
pub const FP16_ENCODER_FILE: &str = "encoder-model.fp16.onnx";

/// The optional Parakeet encoder compiled for Core ML, unpacked from its download.
pub const COREML_ENCODER_DIR: &str = "encoder-model.fp16.mlmodelc";

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

/// Whether `dir` holds an encoder Core ML can run.
pub fn has_accelerated_encoder(dir: &Path) -> bool {
    Route::PREFERENCE
        .iter()
        .any(|route| route.model(dir).exists())
}

/// Which routes to Core ML a load tries, and the records that rule them out.
#[cfg_attr(not(all(feature = "coreml", target_os = "macos")), allow(dead_code))]
mod route {
    use std::path::{Path, PathBuf};

    use super::{COREML_ENCODER_DIR, FP16_ENCODER_FILE};

    /// Holds "cpu" once a route to Core ML has proven slower than or different from the CPU, so later
    /// loads skip it.
    const ONNX_VERDICT_FILE: &str = "verdict";
    const PACKAGE_VERDICT_FILE: &str = "verdict-package";
    /// Count a route's Core ML loads that haven't finished, so one that keeps crashing (or never
    /// finishes) sends later launches past it.
    const ONNX_LOADING_MARKER: &str = "loading";
    const PACKAGE_LOADING_MARKER: &str = "loading-package";
    const MAX_UNFINISHED_LOADS: u32 = 3;

    /// How the encoder reaches Core ML.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Route {
        /// The precompiled Core ML model, run natively: nothing to convert, and Core ML specializes
        /// each input length for the Neural Engine in seconds.
        Package,
        /// The fp16 ONNX encoder on ONNX Runtime's Core ML execution provider, which converts and
        /// compiles it on first load, for about a minute per input length.
        Onnx,
    }

    impl Route {
        /// Best first.
        pub(super) const PREFERENCE: [Route; 2] = [Route::Package, Route::Onnx];

        pub(super) fn model(self, dir: &Path) -> PathBuf {
            match self {
                Self::Package => dir.join(COREML_ENCODER_DIR),
                Self::Onnx => dir.join(FP16_ENCODER_FILE),
            }
        }

        pub(super) fn verdict_file(self) -> &'static str {
            match self {
                Self::Package => PACKAGE_VERDICT_FILE,
                Self::Onnx => ONNX_VERDICT_FILE,
            }
        }

        pub(super) fn loading_marker(self) -> &'static str {
            match self {
                Self::Package => PACKAGE_LOADING_MARKER,
                Self::Onnx => ONNX_LOADING_MARKER,
            }
        }
    }

    /// Core ML's compiled-model cache and load records for the model in `dir`, beside the model
    /// rather than in it so the model store's files stay as downloaded.
    pub(super) fn cache_dir(dir: &Path) -> PathBuf {
        let name = dir.file_name().map_or_else(
            || "model".into(),
            |name| name.to_string_lossy().into_owned(),
        );
        dir.with_file_name(format!("{name}.coreml-cache"))
    }

    pub(super) fn unfinished_loads(cache: &Path, route: Route) -> u32 {
        std::fs::read_to_string(cache.join(route.loading_marker()))
            .ok()
            .and_then(|count| count.trim().parse().ok())
            .unwrap_or(0)
    }

    /// The routes to Core ML worth trying for the model in `dir`, best first: those whose model is
    /// there and that haven't lost to the CPU or kept failing to finish loading. `package_supported`
    /// is whether this macOS can load the precompiled model.
    pub(super) fn routes(dir: &Path, cache: &Path, package_supported: bool) -> Vec<Route> {
        Route::PREFERENCE
            .into_iter()
            .filter(|&route| {
                if !route.model(dir).exists() {
                    return false;
                }
                if route == Route::Package && !package_supported {
                    log::info!("Parakeet: this macOS can't load the precompiled Core ML encoder");
                    return false;
                }
                let verdict = cache.join(route.verdict_file());
                if std::fs::read_to_string(verdict).is_ok_and(|verdict| verdict == "cpu") {
                    log::info!("Parakeet: skipping Core ML ({route:?}), which the CPU beat on this Mac");
                    return false;
                }
                let unfinished = unfinished_loads(cache, route);
                if unfinished >= MAX_UNFINISHED_LOADS {
                    log::warn!(
                        "Parakeet: {unfinished} Core ML loads ({route:?}) didn't finish, skipping it \
                         (delete {} to retry)",
                        cache.join(route.loading_marker()).display()
                    );
                    return false;
                }
                true
            })
            .collect()
    }
}

impl TdtModel {
    /// Loads the model in `dir` with the process's ONNX Runtime environment, on the CPU. With
    /// [`Acceleration::Auto`] and an encoder Core ML runs in `dir`, the encoder also loads on
    /// Core ML in the background, one input length at a time, and takes over inputs that fit a
    /// loaded length once it encodes like the CPU encoder, only faster. The decoder and joint
    /// network always run on the CPU, which is faster for their tiny per-step calls.
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
mod package;

#[cfg(all(feature = "coreml", target_os = "macos"))]
mod core_ml {
    use std::panic::AssertUnwindSafe;
    use std::path::Path;
    use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
    use std::time::Duration;

    use instant::Instant;

    use parakeet_rs::ort::ep::coreml::{ComputeUnits, CoreML, ModelFormat};
    use parakeet_rs::ort::inputs;
    use parakeet_rs::ort::session::Session;
    use parakeet_rs::ort::session::builder::GraphOptimizationLevel;
    use parakeet_rs::ort::value::Tensor;

    use super::route::{Route, cache_dir, routes, unfinished_loads};
    use super::{EncoderDevice, TdtModel, package};
    use crate::TranscribeError;
    use crate::tdt::{N_MELS, cosine_similarity, encoder_output, engine_error};

    /// Input lengths in feature frames (10 ms each) that Core ML runs the encoder at, loaded
    /// shortest first: the Neural Engine runs only static shapes, and a shorter input runs faster
    /// padded to the smallest length that fits. 30 s covers the longest utterance the stream
    /// hands over. Core ML placed these sizes on the Neural Engine on an M4 Max but put 800 and
    /// 1000 frames on the GPU. The precompiled model has a function for each, `frames_<n>`.
    const BUCKET_FRAMES: [usize; 3] = [500, 1500, 3000];
    /// Core ML must reproduce the CPU encoder's output on the probe this closely (cosine
    /// similarity). Low because the CPU encoder is int8: on speech, the fp16 Core ML encoder
    /// matches the fp32 encoder at 0.97 or better while int8 matches it at only 0.73-0.80, and the
    /// synthetic probe lands near 0.75, so this only rules out broken output.
    const MIN_SIMILARITY: f32 = 0.6;

    /// The encoder on Core ML at one input length.
    struct Bucket {
        frames: usize,
        encoder: BucketEncoder,
    }

    enum BucketEncoder {
        Package(package::Function),
        Onnx(Session),
    }

    impl Bucket {
        fn load(route: Route, dir: &Path, frames: usize) -> Result<Self, TranscribeError> {
            let model = route.model(dir);
            let encoder = match route {
                Route::Package => BucketEncoder::Package(package::Function::load(&model, frames)?),
                Route::Onnx => BucketEncoder::Onnx(load_onnx_bucket(&model, dir, frames)?),
            };
            let mut bucket = Self { frames, encoder };
            // The first run specializes the model for the device, which takes a while.
            bucket.encode(&vec![0.0; N_MELS * frames], frames)?;
            Ok(bucket)
        }

        /// Encodes `features`, `[N_MELS][frames]` row-major, padded to this bucket's length.
        fn encode(
            &mut self,
            features: &[f32],
            frames: usize,
        ) -> Result<(Vec<f32>, usize, usize), TranscribeError> {
            let bucket = self.frames;
            let mut padded = vec![0.0f32; N_MELS * bucket];
            for (row, mel) in padded
                .chunks_exact_mut(bucket)
                .zip(features.chunks_exact(frames))
            {
                row[..frames].copy_from_slice(mel);
            }
            match &mut self.encoder {
                BucketEncoder::Package(function) => function.encode(&padded, frames),
                BucketEncoder::Onnx(session) => {
                    let signal = Tensor::from_array(([1usize, N_MELS, bucket], padded))
                        .map_err(engine_error)?;
                    let length = Tensor::from_array(([1usize], vec![frames as i64]))
                        .map_err(engine_error)?;
                    let outputs = session
                        .run(inputs!["audio_signal" => signal, "length" => length])
                        .map_err(engine_error)?;
                    encoder_output(&outputs)
                }
            }
        }
    }

    /// The encoder on Core ML: the input lengths loaded so far, and the background load while
    /// more are coming.
    pub struct Accelerator {
        buckets: Vec<Bucket>,
        loading: Option<Receiver<Bucket>>,
    }

    impl Accelerator {
        pub(in crate::tdt) fn off() -> Self {
            Self {
                buckets: Vec::new(),
                loading: None,
            }
        }

        pub(in crate::tdt) fn device(&self) -> EncoderDevice {
            if self.buckets.is_empty() {
                EncoderDevice::Cpu
            } else {
                EncoderDevice::CoreMl
            }
        }

        /// Whether more input lengths may still arrive from the background load.
        pub(in crate::tdt) fn is_loading(&self) -> bool {
            self.loading.is_some()
        }

        pub(in crate::tdt) fn wait(&mut self) {
            while let Some(loading) = &self.loading {
                match loading.recv() {
                    Ok(bucket) => self.buckets.push(bucket),
                    Err(_) => self.loading = None,
                }
            }
        }

        fn poll(&mut self) {
            while let Some(loading) = &self.loading {
                match loading.try_recv() {
                    Ok(bucket) => self.buckets.push(bucket),
                    Err(TryRecvError::Disconnected) => self.loading = None,
                    Err(TryRecvError::Empty) => break,
                }
            }
        }

        /// Encodes `features` on Core ML, or returns `None` when no loaded input length fits
        /// them.
        pub(in crate::tdt) fn encode(
            &mut self,
            features: &[f32],
            frames: usize,
        ) -> Result<Option<(Vec<f32>, usize, usize)>, TranscribeError> {
            self.poll();
            let Some(bucket) = self
                .buckets
                .iter_mut()
                .filter(|bucket| bucket.frames >= frames)
                .min_by_key(|bucket| bucket.frames)
            else {
                return Ok(None);
            };
            bucket.encode(features, frames).map(Some)
        }
    }

    /// Starts loading the encoder in `dir` on Core ML in the background, by the best route that
    /// works, checked against `model`'s CPU encoder.
    pub(super) fn start(model: &mut TdtModel, dir: &Path) -> Accelerator {
        let cache = cache_dir(dir);
        let routes = routes(dir, &cache, package::is_supported());
        if routes.is_empty() {
            return Accelerator::off();
        }
        if let Err(error) = std::fs::create_dir_all(&cache) {
            log::warn!("Parakeet: no Core ML cache, keeping the encoder on the CPU: {error}");
            return Accelerator::off();
        }
        let probe = match Probe::new(model) {
            Ok(probe) => probe,
            Err(error) => {
                log::warn!("Parakeet: encoder stays on the CPU: {error}");
                return Accelerator::off();
            }
        };
        let dir = dir.to_owned();
        let (sender, receiver) = channel();
        let spawned = std::thread::Builder::new()
            .name("parakeet-coreml".to_owned())
            .spawn(move || {
                super::lower_thread_priority();
                for route in routes {
                    let marker = cache.join(route.loading_marker());
                    let unfinished = unfinished_loads(&cache, route);
                    if let Err(error) = std::fs::write(&marker, (unfinished + 1).to_string()) {
                        log::warn!("Parakeet: can't record the Core ML load: {error}");
                        return;
                    }
                    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        load(route, &dir, &probe, &sender)
                    }))
                    .unwrap_or_else(|_| Err(TranscribeError::Engine("Core ML panicked".into())));
                    let _ = std::fs::remove_file(&marker);
                    match result {
                        Ok(Outcome::Ready) => return,
                        Ok(Outcome::Lost(reason)) => {
                            log::warn!("Parakeet: not using Core ML ({route:?}): {reason}");
                            let _ = std::fs::write(cache.join(route.verdict_file()), "cpu");
                        }
                        Err(error) => {
                            log::warn!("Parakeet: Core ML ({route:?}) failed to load: {error}");
                        }
                    }
                }
                log::warn!("Parakeet: encoder stays on the CPU");
            });
        match spawned {
            Ok(_) => Accelerator {
                buckets: Vec::new(),
                loading: Some(receiver),
            },
            Err(error) => {
                log::warn!("Parakeet: encoder stays on the CPU: {error}");
                Accelerator::off()
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

    enum Outcome {
        /// At least the shortest input length went to the encoder.
        Ready,
        /// Core ML lost to the CPU, for the given reason.
        Lost(String),
    }

    /// Loads the encoder by `route`, one input length at a time, sending each to `sender` once
    /// it's ready. The shortest length must first match and beat the CPU on the probe.
    fn load(
        route: Route,
        dir: &Path,
        probe: &Probe,
        sender: &Sender<Bucket>,
    ) -> Result<Outcome, TranscribeError> {
        let started = Instant::now();
        let [shortest, longer @ ..] = BUCKET_FRAMES;
        let mut bucket = Bucket::load(route, dir, shortest)?;
        let (actual, _, _) = bucket.encode(&probe.features, probe.frames)?;
        let similarity = cosine_similarity(&probe.expected, &actual);
        if actual.len() != probe.expected.len() || similarity < MIN_SIMILARITY {
            return Ok(Outcome::Lost(format!(
                "its output differs from the CPU's (similarity {similarity:.4})"
            )));
        }
        let timed = Instant::now();
        bucket.encode(&probe.features, probe.frames)?;
        let core_ml = timed.elapsed();
        if core_ml >= probe.cpu_time {
            return Ok(Outcome::Lost(format!(
                "the CPU beat it ({:?} vs {core_ml:?})",
                probe.cpu_time
            )));
        }
        log::info!(
            "Parakeet: encoder on Core ML ({route:?}) for inputs up to {shortest} frames, loaded \
             and checked in {:?}",
            started.elapsed()
        );
        if sender.send(bucket).is_err() {
            return Ok(Outcome::Ready);
        }
        for frames in longer {
            match Bucket::load(route, dir, frames) {
                Ok(bucket) => {
                    log::info!(
                        "Parakeet: encoder on Core ML ({route:?}) for inputs up to {frames} \
                         frames after {:?}",
                        started.elapsed()
                    );
                    if sender.send(bucket).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    log::warn!(
                        "Parakeet: inputs over the Core ML encoder's loaded lengths stay on the \
                         CPU: {error}"
                    );
                    break;
                }
            }
        }
        Ok(Outcome::Ready)
    }

    fn load_onnx_bucket(
        encoder: &Path,
        dir: &Path,
        frames: usize,
    ) -> Result<Session, TranscribeError> {
        let cache = cache_dir(dir).join(format!("fp16-{frames}"));
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

        pub(in crate::tdt) fn is_loading(&self) -> bool {
            false
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

/// Runs the calling thread, and the Core ML compile work it starts, at utility QoS: dictation keeps
/// running on the CPU meanwhile, so the one-time compile must not compete with the user's work.
/// Background QoS made it four times slower (46 s instead of 12 s per input length on an M4 Max).
#[cfg_attr(not(all(feature = "coreml", target_os = "macos")), allow(dead_code))]
fn lower_thread_priority() {
    #[cfg(target_os = "macos")]
    // SAFETY: only changes the calling thread's QoS class; no pointers are involved.
    unsafe {
        let status = libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
        if status != 0 {
            log::warn!("Parakeet: couldn't lower the Core ML compile priority ({status})");
        }
    }
}

#[cfg(test)]
#[path = "acceleration_tests.rs"]
mod tests;
