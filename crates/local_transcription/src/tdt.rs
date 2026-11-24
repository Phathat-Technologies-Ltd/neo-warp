//! Parakeet TDT on ONNX Runtime with our own decode loop, so decoding can be biased toward
//! context terms ([`crate::bias`]). Feature extraction and the greedy TDT loop follow
//! `parakeet-rs` (NeMo's preprocessing: pre-emphasis, 512-point STFT, 128 Slaney mel bins, log,
//! per-feature normalization); without bias terms the output is the same.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parakeet_rs::ExecutionConfig;
use parakeet_rs::ort::inputs;
use parakeet_rs::ort::session::{Session, SessionOutputs};
use parakeet_rs::ort::value::Tensor;
use realfft::{RealFftPlanner, RealToComplex};

use crate::TranscribeError;
use crate::bias::{
    BiasMatch, BiasState, BoostingTree, PieceVocab, detokenize, render_with_matches,
};

const SAMPLE_RATE: usize = 16_000;
const N_FFT: usize = 512;
const HOP: usize = 160;
const WIN: usize = 400;
const N_MELS: usize = 128;
const PREEMPHASIS: f32 = 0.97;
const MAX_TOKENS_PER_FRAME: usize = 10;
/// Tokens per step, by model probability, that a bias bonus may promote.
const CANDIDATES: usize = 12;
const LSTM_LAYERS: usize = 2;
const LSTM_HIDDEN: usize = 640;

mod acceleration;

use acceleration::Accelerator;
pub use acceleration::{Acceleration, DEFAULT_INTRA_THREADS, EncoderDevice, FP16_ENCODER_FILE};

pub(crate) fn engine_error(error: impl std::fmt::Display) -> TranscribeError {
    TranscribeError::Engine(error.to_string())
}

/// What one decode produced.
#[derive(Debug, Clone, Default)]
pub struct Decoded {
    pub text: String,
    pub tokens: Vec<u32>,
    pub matches: Vec<BiasMatch>,
}

/// Encoder output for one utterance, `[dim][steps]` row-major.
#[derive(Debug, Clone, Default)]
pub struct Encoded {
    data: Vec<f32>,
    dim: usize,
    steps: usize,
}

impl Encoded {
    /// Cosine similarity with `other`, 0 when their shapes differ.
    pub fn similarity(&self, other: &Encoded) -> f32 {
        if (self.dim, self.steps) != (other.dim, other.steps) {
            return 0.0;
        }
        cosine_similarity(&self.data, &other.data)
    }
}

pub(crate) const ENCODER_FILE: &str = "encoder-model.int8.onnx";
const JOINT_FILE: &str = "decoder_joint-model.int8.onnx";

pub struct TdtModel {
    /// The CPU encoder, dropped while Core ML encodes and reloaded for inputs Core ML can't take.
    encoder: Option<Session>,
    dir: PathBuf,
    config: ExecutionConfig,
    accelerator: Accelerator,
    joint: Session,
    vocab: PieceVocab,
    blank: u32,
    mel_basis: Vec<f32>,
    window: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
}

impl TdtModel {
    /// Loads the model in `dir` on the CPU with the process's ONNX Runtime environment.
    pub fn load_cpu(dir: &Path) -> Result<Self, TranscribeError> {
        let config = ExecutionConfig::new().with_environment(crate::ort_env::environment()?);
        Self::load(dir, &config)
    }

    pub fn load(dir: &Path, config: &ExecutionConfig) -> Result<Self, TranscribeError> {
        let encoder = config
            .build_session(&dir.join(ENCODER_FILE))
            .map_err(engine_error)?;
        Self::with_encoder(dir, encoder, config)
    }

    fn cpu_encoder(&mut self) -> Result<&mut Session, TranscribeError> {
        if self.encoder.is_none() {
            log::info!("Parakeet: loading the CPU encoder for an input Core ML can't take");
            let session = self
                .config
                .build_session(&self.dir.join(ENCODER_FILE))
                .map_err(engine_error)?;
            self.encoder = Some(session);
        }
        Ok(self.encoder.as_mut().expect("loaded above"))
    }

    /// Loads the model in `dir` with `encoder` already built from its `encoder-model.int8.onnx`.
    pub(crate) fn with_encoder(
        dir: &Path,
        encoder: Session,
        joint_config: &ExecutionConfig,
    ) -> Result<Self, TranscribeError> {
        let vocab_text = std::fs::read_to_string(dir.join("vocab.txt")).map_err(engine_error)?;
        let vocab = PieceVocab::from_vocab_txt(&vocab_text);
        if vocab.is_empty() {
            return Err(TranscribeError::Engine(
                "the vocabulary is empty".to_owned(),
            ));
        }
        let joint = joint_config
            .build_session(&dir.join(JOINT_FILE))
            .map_err(engine_error)?;
        let blank = vocab.len() as u32 - 1;
        Ok(Self {
            encoder: Some(encoder),
            dir: dir.to_owned(),
            config: joint_config.clone(),
            accelerator: Accelerator::off(),
            joint,
            vocab,
            blank,
            mel_basis: mel_filterbank(),
            window: (0..WIN)
                .map(|i| {
                    0.5 - 0.5 * ((2.0 * std::f32::consts::PI * i as f32) / (WIN as f32 - 1.0)).cos()
                })
                .collect(),
            fft: RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT),
        })
    }

    pub fn vocab(&self) -> &PieceVocab {
        &self.vocab
    }

    /// Transcribes 16 kHz mono `samples`, biased toward `tree` when given.
    pub fn transcribe(
        &mut self,
        samples: &[f32],
        tree: Option<&BoostingTree>,
    ) -> Result<Decoded, TranscribeError> {
        let encoded = self.encode(samples)?;
        self.decode(&encoded, tree)
    }

    /// Runs the encoder over 16 kHz mono `samples`.
    pub fn encode(&mut self, samples: &[f32]) -> Result<Encoded, TranscribeError> {
        let (features, frames) = self.features(samples)?;
        if frames == 0 {
            return Ok(Encoded::default());
        }
        let (data, dim, steps) = self.run_encoder(features, frames)?;
        Ok(Encoded { data, dim, steps })
    }

    /// Decodes encoder output, biased toward `tree` when given.
    pub fn decode(
        &mut self,
        encoded: &Encoded,
        tree: Option<&BoostingTree>,
    ) -> Result<Decoded, TranscribeError> {
        if encoded.steps == 0 {
            return Ok(Decoded::default());
        }
        self.decode_frames(
            &encoded.data,
            encoded.dim,
            encoded.steps,
            tree.filter(|tree| !tree.is_empty()),
        )
    }

    /// Log-mel features, `[N_MELS][frames]` row-major.
    fn features(&self, samples: &[f32]) -> Result<(Vec<f32>, usize), TranscribeError> {
        if samples.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let mut audio = Vec::with_capacity(samples.len());
        audio.push(samples[0]);
        audio.extend(samples.windows(2).map(|w| w[1] - PREEMPHASIS * w[0]));
        let pad = N_FFT / 2;
        let mut padded = vec![0.0f32; pad];
        padded.extend_from_slice(&audio);
        padded.resize(padded.len() + pad, 0.0);
        let frames = audio.len() / HOP;
        let bins = N_FFT / 2 + 1;
        let offset = (N_FFT - WIN) / 2;
        let mut input = vec![0.0f32; N_FFT];
        let mut output = self.fft.make_output_vec();
        let mut scratch = self.fft.make_scratch_vec();
        let mut power = vec![0.0f32; bins];
        let mut mel = vec![0.0f32; N_MELS * frames];
        let guard = 2.0f32.powi(-24);
        for frame in 0..frames {
            let start = frame * HOP;
            input.fill(0.0);
            for i in 0..WIN {
                input[offset + i] = padded[start + offset + i] * self.window[i];
            }
            self.fft
                .process_with_scratch(&mut input, &mut output, &mut scratch)
                .map_err(engine_error)?;
            for (k, value) in output.iter().enumerate().take(bins) {
                power[k] = value.norm_sqr();
            }
            for m in 0..N_MELS {
                let row = &self.mel_basis[m * bins..(m + 1) * bins];
                let energy: f32 = row.iter().zip(&power).map(|(w, p)| w * p).sum();
                mel[m * frames + frame] = (energy + guard).ln();
            }
        }
        if frames > 1 {
            for m in 0..N_MELS {
                let row = &mut mel[m * frames..(m + 1) * frames];
                let mean = row.iter().sum::<f32>() / frames as f32;
                let variance =
                    row.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / (frames as f32 - 1.0);
                let std = variance.sqrt() + 1e-5;
                for value in row.iter_mut() {
                    *value = (*value - mean) / std;
                }
            }
        }
        Ok((mel, frames))
    }

    /// Returns the encoder output `[dim][steps]` row-major, `dim`, and `steps`.
    fn run_encoder(
        &mut self,
        features: Vec<f32>,
        frames: usize,
    ) -> Result<(Vec<f32>, usize, usize), TranscribeError> {
        if let Some(encoded) = self.accelerator.encode(&features, frames)? {
            self.encoder = None;
            return Ok(encoded);
        }
        self.run_cpu_encoder(features, frames)
    }

    fn run_cpu_encoder(
        &mut self,
        features: Vec<f32>,
        frames: usize,
    ) -> Result<(Vec<f32>, usize, usize), TranscribeError> {
        let signal =
            Tensor::from_array(([1usize, N_MELS, frames], features)).map_err(engine_error)?;
        let length = Tensor::from_array(([1usize], vec![frames as i64])).map_err(engine_error)?;
        let outputs = self
            .cpu_encoder()?
            .run(inputs!["audio_signal" => signal, "length" => length])
            .map_err(engine_error)?;
        encoder_output(&outputs)
    }

    fn decode_frames(
        &mut self,
        encoded: &[f32],
        dim: usize,
        steps: usize,
        tree: Option<&BoostingTree>,
    ) -> Result<Decoded, TranscribeError> {
        let vocab_size = self.vocab.len();
        let blank = self.blank;
        let mut state_h = vec![0.0f32; LSTM_LAYERS * LSTM_HIDDEN];
        let mut state_c = vec![0.0f32; LSTM_LAYERS * LSTM_HIDDEN];
        let mut tokens: Vec<u32> = Vec::new();
        let mut matches = Vec::new();
        let mut bias_state = BiasState::default();
        let mut last = blank as i32;
        let mut t = 0;
        let mut emitted = 0;
        let mut frame = vec![0.0f32; dim];
        let mut logprobs = vec![0.0f32; vocab_size];
        while t < steps {
            for (d, value) in frame.iter_mut().enumerate() {
                *value = encoded[d * steps + t];
            }
            let outputs = self
                .joint
                .run(inputs![
                    "encoder_outputs" => Tensor::from_array(([1usize, dim, 1], frame.clone())).map_err(engine_error)?,
                    "targets" => Tensor::from_array(([1usize, 1], vec![last])).map_err(engine_error)?,
                    "target_length" => Tensor::from_array(([1usize], vec![1i32])).map_err(engine_error)?,
                    "input_states_1" => Tensor::from_array(([LSTM_LAYERS, 1, LSTM_HIDDEN], state_h.clone())).map_err(engine_error)?,
                    "input_states_2" => Tensor::from_array(([LSTM_LAYERS, 1, LSTM_HIDDEN], state_c.clone())).map_err(engine_error)?
                ])
                .map_err(engine_error)?;
            let (_, logits) = outputs["outputs"]
                .try_extract_tensor::<f32>()
                .map_err(engine_error)?;
            let split = vocab_size.min(logits.len());
            let (token_logits, duration_logits) = logits.split_at(split);
            let duration = argmax(duration_logits).unwrap_or(0);
            let best = argmax(token_logits).unwrap_or(blank as usize) as u32;
            let token = match tree {
                None => best,
                Some(tree) => {
                    log_softmax(token_logits, &mut logprobs);
                    let best_logprob = logprobs[best as usize];
                    let scoring_state = bias_state.without_leave_penalty();
                    let fused = |token: u32| {
                        let logprob = logprobs[token as usize];
                        let bonus = if token == blank {
                            Some(0.0)
                        } else {
                            tree.bonus(
                                &scoring_state,
                                token,
                                logprob,
                                (best, best_logprob),
                                &self.vocab,
                            )
                        };
                        (logprob + bonus.unwrap_or(0.0), bonus)
                    };
                    let mut chosen = (best, fused(best));
                    if best != blank {
                        let blank_score = fused(blank);
                        if blank_score.0 > chosen.1.0 {
                            chosen = (blank, blank_score);
                        }
                    }
                    for candidate in top_k(&logprobs, CANDIDATES) {
                        if candidate != best && candidate != blank {
                            let score = fused(candidate);
                            if score.0 > chosen.1.0 {
                                chosen = (candidate, score);
                            }
                        }
                    }
                    let (token, (_, bonus)) = chosen;
                    if token != blank {
                        let (next, found) =
                            tree.advance(&bias_state, token, tokens.len(), bonus, &self.vocab);
                        bias_state = next;
                        matches.extend(found);
                    }
                    token
                }
            };
            if token != blank {
                for (name, state) in [
                    ("output_states_1", &mut state_h),
                    ("output_states_2", &mut state_c),
                ] {
                    let (_, data) = outputs[name]
                        .try_extract_tensor::<f32>()
                        .map_err(engine_error)?;
                    let len = state.len();
                    state.copy_from_slice(&data[..len]);
                }
                tokens.push(token);
                last = token as i32;
                emitted += 1;
            }
            if duration > 0 {
                t += duration;
                emitted = 0;
            } else if token == blank || emitted >= MAX_TOKENS_PER_FRAME {
                t += 1;
                emitted = 0;
            }
        }
        let text = match tree {
            Some(tree) if !matches.is_empty() => {
                render_with_matches(&tokens, &matches, tree, &self.vocab)
            }
            _ => detokenize(&tokens, &self.vocab),
        };
        Ok(Decoded {
            text: text.trim().to_owned(),
            tokens,
            matches,
        })
    }
}

/// Reads the encoder's output `[dim][steps]` row-major, trimmed to its valid steps, with `dim` and
/// the number of valid steps.
fn encoder_output(
    outputs: &SessionOutputs<'_>,
) -> Result<(Vec<f32>, usize, usize), TranscribeError> {
    let (shape, data) = outputs["outputs"]
        .try_extract_tensor::<f32>()
        .map_err(engine_error)?;
    let dim = shape[1] as usize;
    let steps = shape[2] as usize;
    let (_, lengths) = outputs["encoded_lengths"]
        .try_extract_tensor::<i64>()
        .map_err(engine_error)?;
    let valid = lengths
        .first()
        .map_or(steps, |&len| (len.max(0) as usize).min(steps));
    let mut encoded = Vec::with_capacity(dim * valid);
    for d in 0..dim {
        encoded.extend_from_slice(&data[d * steps..d * steps + valid]);
    }
    Ok((encoded, dim, valid))
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

fn top_k(values: &[f32], k: usize) -> Vec<u32> {
    let mut top: Vec<(f32, u32)> = Vec::with_capacity(k + 1);
    for (index, &value) in values.iter().enumerate() {
        if top.len() < k || value > top[top.len() - 1].0 {
            let at = top.partition_point(|(v, _)| *v >= value);
            top.insert(at, (value, index as u32));
            top.truncate(k);
        }
    }
    top.into_iter().map(|(_, index)| index).collect()
}

fn argmax(values: &[f32]) -> Option<usize> {
    values
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(index, _)| index)
}

fn log_softmax(logits: &[f32], out: &mut [f32]) {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let sum: f32 = logits.iter().map(|x| (x - max).exp()).sum();
    let log_sum = max + sum.ln();
    for (o, x) in out.iter_mut().zip(logits) {
        *o = x - log_sum;
    }
}

/// librosa's Slaney-normalized mel filterbank, `[N_MELS][N_FFT / 2 + 1]` row-major.
fn mel_filterbank() -> Vec<f32> {
    const F_SP: f64 = 200.0 / 3.0;
    const MIN_LOG_HZ: f64 = 1000.0;
    const MIN_LOG_MEL: f64 = MIN_LOG_HZ / F_SP;
    const LOG_STEP: f64 = 0.068_751_777_420_949_12;
    let hz_to_mel = |hz: f64| {
        if hz < MIN_LOG_HZ {
            hz / F_SP
        } else {
            MIN_LOG_MEL + (hz / MIN_LOG_HZ).ln() / LOG_STEP
        }
    };
    let mel_to_hz = |mel: f64| {
        if mel < MIN_LOG_MEL {
            mel * F_SP
        } else {
            MIN_LOG_HZ * ((mel - MIN_LOG_MEL) * LOG_STEP).exp()
        }
    };
    let bins = N_FFT / 2 + 1;
    let mel_max = hz_to_mel(SAMPLE_RATE as f64 / 2.0);
    let points: Vec<f64> = (0..=N_MELS + 1)
        .map(|i| mel_to_hz(mel_max * i as f64 / (N_MELS + 1) as f64))
        .collect();
    let mut bank = vec![0.0f32; N_MELS * bins];
    for m in 0..N_MELS {
        let enorm = 2.0 / (points[m + 2] - points[m]);
        for k in 0..bins {
            let freq = k as f64 * SAMPLE_RATE as f64 / N_FFT as f64;
            let lower = (freq - points[m]) / (points[m + 1] - points[m]);
            let upper = (points[m + 2] - freq) / (points[m + 2] - points[m + 1]);
            bank[m * bins + k] = (0.0f64.max(lower.min(upper)) * enorm) as f32;
        }
    }
    bank
}
