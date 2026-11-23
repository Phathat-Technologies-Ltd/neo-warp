use std::path::Path;

use parakeet_rs::ExecutionConfig;
use serde::{Deserialize, Serialize};

use super::TdtModel;
use crate::TranscribeError;

/// ONNX Runtime intra-op threads for Parakeet on the CPU. More threads barely shorten a decode but
/// keep more cores busy while the user talks.
pub const DEFAULT_INTRA_THREADS: usize = 3;

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
    /// Loads the model in `dir` with the process's ONNX Runtime environment, putting the encoder
    /// where `acceleration` asks and falling back to the CPU when that fails. The decoder and joint
    /// network always run on the CPU, which is faster for their tiny per-step calls.
    pub fn load_accelerated(
        dir: &Path,
        acceleration: Acceleration,
        intra_threads: Option<usize>,
    ) -> Result<(Self, EncoderDevice), TranscribeError> {
        let config = ExecutionConfig::new()
            .with_environment(crate::ort_env::environment()?)
            .with_intra_threads(intra_threads.unwrap_or(DEFAULT_INTRA_THREADS));
        let model = Self::load(dir, &config)?;
        match acceleration {
            Acceleration::Cpu => Ok((model, EncoderDevice::Cpu)),
            Acceleration::Auto => Ok(core_ml::accelerate(model, dir)),
        }
    }
}

#[cfg(all(feature = "coreml", target_os = "macos"))]
mod core_ml {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use instant::Instant;

    use parakeet_rs::ort::ep::coreml::{ComputeUnits, CoreML, ModelFormat};
    use parakeet_rs::ort::session::Session;
    use parakeet_rs::ort::session::builder::GraphOptimizationLevel;

    use super::{EncoderDevice, TdtModel};
    use crate::TranscribeError;
    use crate::tdt::{ENCODER_FILE, engine_error};

    /// Core ML's compiled-model cache for the model in `dir`, beside the model rather than in it
    /// so the model store's files stay as downloaded.
    fn cache_dir(dir: &Path) -> PathBuf {
        let name = dir.file_name().map_or_else(
            || "model".into(),
            |name| name.to_string_lossy().into_owned(),
        );
        dir.with_file_name(format!("{name}.coreml-cache"))
    }

    /// Core ML must reproduce the CPU encoder's output this closely (cosine similarity).
    const MIN_SIMILARITY: f32 = 0.99;
    /// Holds "cpu" once Core ML has proven slower than the CPU, so later loads skip compiling it.
    const VERDICT_FILE: &str = "verdict";
    /// Exists while Core ML loads, so a crash inside Core ML sends the next launch to the CPU.
    const LOADING_MARKER: &str = "loading";

    pub(super) fn accelerate(mut model: TdtModel, dir: &Path) -> (TdtModel, EncoderDevice) {
        let cache = cache_dir(dir);
        let marker = cache.join(LOADING_MARKER);
        if std::fs::read_to_string(cache.join(VERDICT_FILE)).is_ok_and(|verdict| verdict == "cpu") {
            log::info!("Parakeet: encoder on the CPU, which beat Core ML on this Mac");
            return (model, EncoderDevice::Cpu);
        }
        if marker.exists() {
            log::warn!(
                "Parakeet: a previous Core ML load didn't finish, keeping the encoder on the CPU \
                 (delete {} to retry)",
                marker.display()
            );
            return (model, EncoderDevice::Cpu);
        }
        if let Err(error) =
            std::fs::create_dir_all(&cache).and_then(|()| std::fs::write(&marker, b""))
        {
            log::warn!("Parakeet: no Core ML cache, keeping the encoder on the CPU: {error}");
            return (model, EncoderDevice::Cpu);
        }
        let started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let encoder = load_encoder(dir, &cache)?;
            swap_if_better(&mut model, encoder)
        }))
        .unwrap_or_else(|_| Err(TranscribeError::Engine("Core ML panicked".to_owned())));
        let _ = std::fs::remove_file(&marker);
        match result {
            Ok(Verdict::CoreMl) => {
                log::info!(
                    "Parakeet: encoder on Core ML (loaded and checked in {:?})",
                    started.elapsed()
                );
                (model, EncoderDevice::CoreMl)
            }
            Ok(Verdict::Cpu { cpu, core_ml }) => {
                log::info!(
                    "Parakeet: encoder on the CPU, which beat Core ML ({cpu:?} vs {core_ml:?})"
                );
                let _ = std::fs::write(cache.join(VERDICT_FILE), "cpu");
                (model, EncoderDevice::Cpu)
            }
            Err(error) => {
                log::warn!(
                    "Parakeet: Core ML unavailable, keeping the encoder on the CPU: {error}"
                );
                (model, EncoderDevice::Cpu)
            }
        }
    }

    fn load_encoder(dir: &Path, cache: &Path) -> Result<Session, TranscribeError> {
        let core_ml = CoreML::default()
            .with_model_format(ModelFormat::NeuralNetwork)
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
            .with_execution_providers([core_ml])
            .map_err(engine_error)?
            .commit_from_file(dir.join(ENCODER_FILE))
            .map_err(engine_error)
    }

    enum Verdict {
        CoreMl,
        Cpu { cpu: Duration, core_ml: Duration },
    }

    /// Puts `encoder` in `model` if it encodes a probe like the CPU encoder does, and faster.
    fn swap_if_better(model: &mut TdtModel, encoder: Session) -> Result<Verdict, TranscribeError> {
        let probe = probe_audio();
        let (features, frames) = model.features(&probe)?;
        let (expected, _, expected_steps) = model.run_encoder(features.clone(), frames)?;
        let started = Instant::now();
        model.run_encoder(features.clone(), frames)?;
        let cpu_time = started.elapsed();
        let cpu = std::mem::replace(&mut model.encoder, encoder);
        let checked = model.run_encoder(features.clone(), frames).and_then(|(actual, _, steps)| {
            let similarity = cosine_similarity(&expected, &actual);
            if steps != expected_steps || similarity < MIN_SIMILARITY {
                return Err(TranscribeError::Engine(format!(
                    "Core ML output differs from the CPU (similarity {similarity:.4}, {steps} vs \
                     {expected_steps} steps)"
                )));
            }
            let started = Instant::now();
            model.run_encoder(features, frames)?;
            let core_ml = started.elapsed();
            Ok(if core_ml < cpu_time {
                Verdict::CoreMl
            } else {
                Verdict::Cpu {
                    cpu: cpu_time,
                    core_ml,
                }
            })
        });
        if !matches!(checked, Ok(Verdict::CoreMl)) {
            model.encoder = cpu;
        }
        checked
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

    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() || a.is_empty() {
            return 0.0;
        }
        let dot: f64 = a
            .iter()
            .zip(b)
            .map(|(x, y)| f64::from(*x) * f64::from(*y))
            .sum();
        let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
        let similarity = dot / (norm(a) * norm(b));
        if similarity.is_finite() {
            similarity as f32
        } else {
            0.0
        }
    }
}

#[cfg(not(all(feature = "coreml", target_os = "macos")))]
mod core_ml {
    use std::path::Path;

    use super::{EncoderDevice, TdtModel};

    pub(super) fn accelerate(model: TdtModel, _dir: &Path) -> (TdtModel, EncoderDevice) {
        (model, EncoderDevice::Cpu)
    }
}
