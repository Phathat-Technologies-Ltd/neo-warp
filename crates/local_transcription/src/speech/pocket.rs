use std::collections::HashMap;
use std::sync::Mutex;

use parakeet_rs::ort::session::builder::AutoDevicePolicy;
use parakeet_rs::ort::session::{Session, SessionInputValue};
use parakeet_rs::ort::value::{DynValue, Tensor, TensorElementType, ValueType};

use super::{SpeakError, Speaker, VoiceId, VoiceInfo};
use crate::{Audio, ModelStore};

pub const POCKET_LM_MAIN_FILE: &str = "lm_main.int8.onnx";
pub const POCKET_LM_FLOW_FILE: &str = "lm_flow.int8.onnx";
pub const POCKET_DECODER_FILE: &str = "decoder.int8.onnx";
pub const POCKET_ENCODER_FILE: &str = "encoder.onnx";
pub const POCKET_TEXT_CONDITIONER_FILE: &str = "text_conditioner.onnx";
pub const POCKET_VOCAB_FILE: &str = "vocab.json";
pub const POCKET_SCORES_FILE: &str = "token_scores.json";

const SAMPLE_RATE: u32 = 24_000;
const LATENT_DIM: usize = 32;
const TEXT_DIM: usize = 1024;
const FLOW_STEPS: usize = 4;
/// 40 s of audio at 12.5 frames per second.
const MAX_FRAMES: usize = 500;
/// Frames still generated after the end-of-speech logit fires, so the last word isn't clipped.
const FRAMES_AFTER_EOS: usize = 3;
/// The logit above which the model is taken to have finished the sentence.
const EOS_THRESHOLD: f32 = -4.0;
const TEMPERATURE: f32 = 0.7;
const DECODE_CHUNK_FRAMES: usize = 15;

/// The voice baked into the export: Alba MacKenna (CC BY 4.0, kyutai/tts-voices).
/// The credit the CC BY 4.0 license of the Pocket TTS weights and its voice requires.
pub const POCKET_ATTRIBUTION: &str =
    "Pocket TTS by Kyutai, voice by Alba MacKenna, licensed CC BY 4.0.";

pub const POCKET_VOICES: &[(&str, &str)] = &[("alba", "Alba (UK female)")];

/// Kyutai Pocket TTS (100M parameters, CC BY 4.0), int8 ONNX split into the five graphs of the
/// sherpa-onnx export, on ONNX Runtime (CPU). It generates 80 ms latent frames autoregressively
/// and decodes them with the Mimi codec.
pub struct PocketSpeaker {
    store: ModelStore,
    loaded: Mutex<Option<Loaded>>,
}

struct Loaded {
    lm_main: Session,
    lm_flow: Session,
    decoder: Session,
    text_conditioner: Session,
    tokenizer: Tokenizer,
    /// The voice prompt the language model is conditioned on before each sentence.
    voice: (Vec<usize>, Vec<f32>),
    rng: Normal,
}

fn engine_error(error: impl std::fmt::Display) -> SpeakError {
    SpeakError::Engine(error.to_string())
}

impl PocketSpeaker {
    pub fn new(store: ModelStore) -> Self {
        Self {
            store,
            loaded: Mutex::new(None),
        }
    }

    fn load(&self) -> Result<Loaded, SpeakError> {
        let dir = self.store.model_dir();
        let environment = crate::ort_env::environment().map_err(engine_error)?;
        let open = |file: &str| {
            Session::builder(&environment)
                .and_then(|mut builder| {
                    // One thread keeps speech under a core; the model still runs ~5x real time.
                    builder = builder.with_intra_threads(1)?;
                    builder = builder.with_inter_threads(1)?;
                    // ONNX Runtime registers CoreML on macOS by default, which rejects the empty
                    // sequences that condition the language model.
                    builder = builder.with_auto_device(AutoDevicePolicy::PreferCPU)?;
                    builder.commit_from_file(dir.join(file))
                })
                .map_err(engine_error)
        };
        let mut encoder = open(POCKET_ENCODER_FILE)?;
        let vocab = std::fs::read_to_string(dir.join(POCKET_VOCAB_FILE)).map_err(engine_error)?;
        let scores = std::fs::read_to_string(dir.join(POCKET_SCORES_FILE)).map_err(engine_error)?;
        // The export's encoder ignores its audio and returns the baked-in voice's prompt.
        let silence = Tensor::from_array(([1, 1, SAMPLE_RATE as usize], vec![0f32; 24_000]))
            .map_err(engine_error)?;
        let outputs = encoder
            .run(vec![("audio", SessionInputValue::from(silence))])
            .map_err(engine_error)?;
        let voice = owned_f32(&outputs[0])?;
        drop(outputs);
        Ok(Loaded {
            lm_main: open(POCKET_LM_MAIN_FILE)?,
            lm_flow: open(POCKET_LM_FLOW_FILE)?,
            decoder: open(POCKET_DECODER_FILE)?,
            text_conditioner: open(POCKET_TEXT_CONDITIONER_FILE)?,
            tokenizer: Tokenizer::parse(&vocab, &scores)?,
            voice,
            rng: Normal::new(0x5eed_1234_abcd_0001),
        })
    }
}

fn owned_f32(value: &DynValue) -> Result<(Vec<usize>, Vec<f32>), SpeakError> {
    let (shape, data) = value.try_extract_tensor::<f32>().map_err(engine_error)?;
    let shape = shape.iter().map(|dim| *dim as usize).collect();
    Ok((shape, data.to_vec()))
}

fn f32_tensor(shape: &[usize], data: Vec<f32>) -> Result<DynValue, SpeakError> {
    Ok(Tensor::from_array((shape.to_vec(), data))
        .map_err(engine_error)?
        .into_dyn())
}

/// Zero-filled values for a stateful graph's state inputs (every input from `first` on), with
/// dynamic dimensions set to 1.
fn initial_state(session: &Session, first: usize) -> Result<Vec<DynValue>, SpeakError> {
    session.inputs()[first..]
        .iter()
        .map(|input| {
            let ValueType::Tensor { ty, shape, .. } = input.dtype() else {
                return Err(SpeakError::Engine(format!(
                    "state input {} isn't a tensor",
                    input.name()
                )));
            };
            let dims: Vec<usize> = shape
                .iter()
                .map(|dim| if *dim < 0 { 1 } else { *dim as usize })
                .collect();
            let len = dims.iter().product();
            let value = match ty {
                TensorElementType::Float32 => {
                    Tensor::from_array((dims, vec![0f32; len])).map(Tensor::into_dyn)
                }
                TensorElementType::Int64 => {
                    Tensor::from_array((dims, vec![0i64; len])).map(Tensor::into_dyn)
                }
                TensorElementType::Bool => {
                    Tensor::from_array((dims, vec![false; len])).map(Tensor::into_dyn)
                }
                other => {
                    return Err(SpeakError::Engine(format!(
                        "unsupported state type {other:?}"
                    )));
                }
            };
            value.map_err(engine_error)
        })
        .collect()
}

/// Runs a stateful graph whose inputs are `leading` followed by `state`, and whose outputs are
/// `leading_outputs` values followed by the next state, which replaces `state`.
fn run_stateful(
    session: &mut Session,
    leading: Vec<DynValue>,
    state: &mut Vec<DynValue>,
    leading_outputs: usize,
) -> Result<Vec<DynValue>, SpeakError> {
    let names: Vec<String> = session
        .inputs()
        .iter()
        .map(|input| input.name().to_owned())
        .collect();
    let values = leading.into_iter().chain(state.drain(..));
    let inputs: Vec<(String, SessionInputValue)> = names
        .into_iter()
        .zip(values.map(SessionInputValue::from))
        .collect();
    let outputs = session.run(inputs).map_err(engine_error)?;
    let mut outputs: Vec<DynValue> = outputs.into_iter().map(|(_, value)| value).collect();
    *state = outputs.split_off(leading_outputs);
    Ok(outputs)
}

impl Loaded {
    fn synthesize(&mut self, text: &str) -> Result<Vec<f32>, SpeakError> {
        let ids = self.tokenizer.encode(text);
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let token_ids = Tensor::from_array(([1, ids.len()], ids)).map_err(engine_error)?;
        let outputs = self
            .text_conditioner
            .run(vec![("token_ids", SessionInputValue::from(token_ids))])
            .map_err(engine_error)?;
        let (text_shape, text_embedding) = owned_f32(&outputs[0])?;
        drop(outputs);

        let mut lm_state = initial_state(&self.lm_main, 2)?;
        let empty_sequence = || f32_tensor(&[1, 0, LATENT_DIM], Vec::new());
        let (voice_shape, voice) = &self.voice;
        for (shape, embedding) in [
            (voice_shape.clone(), voice.clone()),
            (text_shape, text_embedding),
        ] {
            run_stateful(
                &mut self.lm_main,
                vec![empty_sequence()?, f32_tensor(&shape, embedding)?],
                &mut lm_state,
                2,
            )?;
        }

        let stddev = TEMPERATURE.sqrt();
        let mut previous = vec![f32::NAN; LATENT_DIM];
        let mut latents = Vec::new();
        let mut eos_step = None;
        for step in 0..MAX_FRAMES {
            let outputs = run_stateful(
                &mut self.lm_main,
                vec![
                    f32_tensor(&[1, 1, LATENT_DIM], previous)?,
                    f32_tensor(&[1, 0, TEXT_DIM], Vec::new())?,
                ],
                &mut lm_state,
                2,
            )?;
            let (_, conditioning) = owned_f32(&outputs[0])?;
            let (_, eos_logit) = owned_f32(&outputs[1])?;
            if eos_step.is_none() && eos_logit.first().is_some_and(|l| *l > EOS_THRESHOLD) {
                eos_step = Some(step);
            }
            if eos_step.is_some_and(|eos| eos > 0 && step >= eos + FRAMES_AFTER_EOS) {
                break;
            }
            let mut latent: Vec<f32> = (0..LATENT_DIM)
                .map(|_| self.rng.sample() * stddev)
                .collect();
            let dt = 1.0 / FLOW_STEPS as f32;
            for flow_step in 0..FLOW_STEPS {
                let s = flow_step as f32 * dt;
                let outputs = self
                    .lm_flow
                    .run(vec![
                        ("c", f32_tensor(&[1, TEXT_DIM], conditioning.clone())?),
                        ("s", f32_tensor(&[1, 1], vec![s])?),
                        ("t", f32_tensor(&[1, 1], vec![s + dt])?),
                        ("x", f32_tensor(&[1, LATENT_DIM], latent.clone())?),
                    ])
                    .map_err(engine_error)?;
                let (_, direction) = outputs[0]
                    .try_extract_tensor::<f32>()
                    .map_err(engine_error)?;
                for (x, d) in latent.iter_mut().zip(direction) {
                    *x += d * dt;
                }
            }
            latents.extend_from_slice(&latent);
            previous = latent;
        }
        drop(lm_state);

        let mut decoder_state = initial_state(&self.decoder, 1)?;
        let mut samples = Vec::new();
        for chunk in latents.chunks(DECODE_CHUNK_FRAMES * LATENT_DIM) {
            let frames = chunk.len() / LATENT_DIM;
            let outputs = run_stateful(
                &mut self.decoder,
                vec![f32_tensor(&[1, frames, LATENT_DIM], chunk.to_vec())?],
                &mut decoder_state,
                1,
            )?;
            let (_, audio) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(engine_error)?;
            samples.extend_from_slice(audio);
        }
        Ok(samples)
    }
}

impl Speaker for PocketSpeaker {
    fn synthesize(&self, text: &str, voice: &VoiceId, _rate: f32) -> Result<Audio, SpeakError> {
        if !self.store.is_installed() {
            return Err(SpeakError::ModelNotInstalled);
        }
        if !POCKET_VOICES.iter().any(|(id, _)| *id == voice.as_str()) {
            return Err(SpeakError::UnknownVoice(voice.as_str().to_owned()));
        }
        let mut loaded = self
            .loaded
            .lock()
            .map_err(|_| SpeakError::Engine("the Pocket TTS lock is poisoned".to_owned()))?;
        if loaded.is_none() {
            *loaded = Some(self.load()?);
        }
        let samples = loaded.as_mut().expect("loaded above").synthesize(text)?;
        Ok(Audio {
            samples,
            sample_rate: SAMPLE_RATE,
        })
    }

    fn is_ready(&self) -> bool {
        self.store.is_installed()
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        POCKET_VOICES
            .iter()
            .map(|(id, name)| VoiceInfo {
                id: VoiceId::new(*id),
                display_name: (*name).to_owned(),
            })
            .collect()
    }
}

/// SentencePiece unigram tokenizer with byte fallback, from the export's vocabulary and piece
/// scores.
pub(crate) struct Tokenizer {
    pieces: HashMap<Vec<u8>, (i64, f32)>,
    bytes: [Option<(i64, f32)>; 256],
    max_piece_len: usize,
}

impl Tokenizer {
    pub(crate) fn parse(vocab: &str, scores: &str) -> Result<Self, SpeakError> {
        let vocab: HashMap<String, i64> = serde_json::from_str(vocab).map_err(engine_error)?;
        let scores: HashMap<String, f32> = serde_json::from_str(scores).map_err(engine_error)?;
        let mut bytes = [None; 256];
        let mut pieces = HashMap::with_capacity(vocab.len());
        for (piece, id) in vocab {
            let score = scores.get(&piece).copied().unwrap_or(0.0);
            if let Some(byte) = piece
                .strip_prefix("<0x")
                .and_then(|rest| rest.strip_suffix('>'))
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            {
                bytes[usize::from(byte)] = Some((id, score));
            }
            pieces.insert(piece.into_bytes(), (id, score));
        }
        let max_piece_len = pieces.keys().map(Vec::len).max().unwrap_or(1);
        Ok(Self {
            pieces,
            bytes,
            max_piece_len,
        })
    }

    /// The highest-scoring segmentation of `text`, with spaces as `▁` and a leading `▁`.
    pub(crate) fn encode(&self, text: &str) -> Vec<i64> {
        let mut normalized = text.replace(' ', "▁");
        if !normalized.starts_with('▁') {
            normalized.insert(0, '▁');
        }
        let text = normalized.as_bytes();
        let n = text.len();
        let mut best = vec![f32::NEG_INFINITY; n + 1];
        let mut next: Vec<Option<(usize, i64)>> = vec![None; n + 1];
        best[n] = 0.0;
        for i in (0..n).rev() {
            for j in (i + 1)..=n.min(i + self.max_piece_len) {
                if let Some((id, score)) = self.pieces.get(&text[i..j]) {
                    let total = score + best[j];
                    if total > best[i] {
                        best[i] = total;
                        next[i] = Some((j, *id));
                    }
                }
            }
            if next[i].is_none() {
                let (id, score) = self.bytes[usize::from(text[i])].unwrap_or((0, 0.0));
                best[i] = score + best[i + 1];
                next[i] = Some((i + 1, id));
            }
        }
        let mut ids = Vec::new();
        let mut i = 0;
        while let Some((j, id)) = next.get(i).copied().flatten() {
            ids.push(id);
            i = j;
        }
        ids
    }
}

/// Standard normal samples (xorshift64* with Box-Muller), so generation needs no RNG crate.
struct Normal {
    state: u64,
    spare: Option<f32>,
}

impl Normal {
    fn new(seed: u64) -> Self {
        Self {
            state: seed.max(1),
            spare: None,
        }
    }

    fn uniform(&mut self) -> f32 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        let bits = self.state.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 40;
        (bits as f32 + 0.5) / (1u64 << 24) as f32
    }

    fn sample(&mut self) -> f32 {
        if let Some(spare) = self.spare.take() {
            return spare;
        }
        let radius = (-2.0 * self.uniform().ln()).sqrt();
        let angle = std::f32::consts::TAU * self.uniform();
        self.spare = Some(radius * angle.sin());
        radius * angle.cos()
    }
}

#[cfg(test)]
#[path = "pocket_tests.rs"]
mod tests;
