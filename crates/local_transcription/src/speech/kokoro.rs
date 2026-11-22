use std::collections::HashMap;
use std::sync::Mutex;

use parakeet_rs::ort::inputs;
use parakeet_rs::ort::session::Session;
use parakeet_rs::ort::value::Tensor;

use super::g2p::{G2p, G2pToken, Phone};
use super::{SpeakError, Speaker, VoiceId, VoiceInfo};
use crate::{Audio, ModelStore};

pub const KOKORO_MODEL_FILE: &str = "model_quantized.onnx";
pub const KOKORO_TOKENIZER_FILE: &str = "tokenizer.json";
const SAMPLE_RATE: u32 = 24_000;
const STYLE_DIM: usize = 256;
/// Phoneme tokens per call; the voice files hold one style row per token count up to this.
const MAX_TOKENS: usize = 510;

/// The voices bundled with [`crate::kokoro_82m_v1_int8`], the default first, as (id, name).
pub const KOKORO_VOICES: &[(&str, &str)] = &[
    ("af_heart", "Heart (US female)"),
    ("af_bella", "Bella (US female)"),
    ("am_michael", "Michael (US male)"),
    ("am_adam", "Adam (US male)"),
];

/// Kokoro-82M v1.0 (Apache-2.0), int8 ONNX, on ONNX Runtime (CPU).
pub struct KokoroSpeaker {
    store: ModelStore,
    loaded: Mutex<Option<Loaded>>,
}

struct Loaded {
    session: Session,
    vocab: HashMap<char, i64>,
    g2p: G2p,
    voices: HashMap<String, Vec<f32>>,
}

fn engine_error(error: impl std::fmt::Display) -> SpeakError {
    SpeakError::Engine(error.to_string())
}

impl KokoroSpeaker {
    pub fn new(store: ModelStore) -> Self {
        Self {
            store,
            loaded: Mutex::new(None),
        }
    }

    fn load(&self) -> Result<Loaded, SpeakError> {
        let dir = self.store.model_dir();
        let environment = crate::ort_env::environment().map_err(engine_error)?;
        let session = Session::builder(&environment)
            .and_then(|mut builder| {
                builder = builder.with_intra_threads(2)?;
                builder.commit_from_file(dir.join(KOKORO_MODEL_FILE))
            })
            .map_err(engine_error)?;
        let tokenizer =
            std::fs::read_to_string(dir.join(KOKORO_TOKENIZER_FILE)).map_err(engine_error)?;
        let vocab = parse_vocab(&tokenizer)?;
        let mut voices = HashMap::new();
        for (id, _) in KOKORO_VOICES {
            let bytes = std::fs::read(dir.join(format!("{id}.bin"))).map_err(engine_error)?;
            voices.insert((*id).to_owned(), le_f32s(&bytes));
        }
        Ok(Loaded {
            session,
            vocab,
            g2p: G2p::load(dir)?,
            voices,
        })
    }
}

/// Reads the phoneme vocabulary from Kokoro's `tokenizer.json`.
fn parse_vocab(tokenizer: &str) -> Result<HashMap<char, i64>, SpeakError> {
    let json: serde_json::Value = serde_json::from_str(tokenizer).map_err(engine_error)?;
    let vocab = json
        .pointer("/model/vocab")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| SpeakError::Engine("tokenizer.json has no vocab".to_owned()))?;
    Ok(vocab
        .iter()
        .filter_map(|(symbol, id)| {
            let mut chars = symbol.chars();
            let c = chars.next()?;
            chars.next().is_none().then_some((c, id.as_i64()?))
        })
        .collect())
}

fn le_f32s(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Maps an ARPAbet phone to Kokoro's (misaki) American English phoneme symbols.
fn misaki(phone: &Phone) -> &'static str {
    let unstressed = phone.stress == Some(0);
    match phone.symbol.as_str() {
        "AA" => "ɑ",
        "AE" => "æ",
        "AH" if unstressed => "ə",
        "AH" => "ʌ",
        "AO" => "ɔ",
        "AW" => "W",
        "AY" => "I",
        "EH" => "ɛ",
        "ER" if unstressed => "əɹ",
        "ER" => "ɜɹ",
        "EY" => "A",
        "IH" => "ɪ",
        "IY" => "i",
        "OW" => "O",
        "OY" => "Y",
        "UH" => "ʊ",
        "UW" => "u",
        "B" => "b",
        "CH" => "ʧ",
        "D" => "d",
        "DH" => "ð",
        "F" => "f",
        "G" => "ɡ",
        "HH" => "h",
        "JH" => "ʤ",
        "K" => "k",
        "L" => "l",
        "M" => "m",
        "N" => "n",
        "NG" => "ŋ",
        "P" => "p",
        "R" => "ɹ",
        "S" => "s",
        "SH" => "ʃ",
        "T" => "t",
        "TH" => "θ",
        "V" => "v",
        "W" => "w",
        "Y" => "j",
        "Z" => "z",
        "ZH" => "ʒ",
        _ => "",
    }
}

/// Builds Kokoro's phoneme string: stress marks before vowels, words separated by spaces.
pub(crate) fn phoneme_string(tokens: &[G2pToken]) -> String {
    let mut out = String::new();
    for token in tokens {
        match token {
            G2pToken::Word(phones) => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                for phone in phones {
                    match phone.stress {
                        Some(1) => out.push('ˈ'),
                        Some(2) => out.push('ˌ'),
                        _ => {}
                    }
                    out.push_str(misaki(phone));
                }
            }
            G2pToken::Punct(mark) => {
                out.push(*mark);
                out.push(' ');
            }
        }
    }
    out.trim_end().to_owned()
}

impl Speaker for KokoroSpeaker {
    fn synthesize(&self, text: &str, voice: &VoiceId, rate: f32) -> Result<Audio, SpeakError> {
        if !self.store.is_installed() {
            return Err(SpeakError::ModelNotInstalled);
        }
        let mut loaded = self
            .loaded
            .lock()
            .map_err(|_| SpeakError::Engine("the Kokoro lock is poisoned".to_owned()))?;
        if loaded.is_none() {
            *loaded = Some(self.load()?);
        }
        let loaded = loaded.as_mut().expect("loaded above");
        let style_table = loaded
            .voices
            .get(voice.as_str())
            .ok_or_else(|| SpeakError::UnknownVoice(voice.as_str().to_owned()))?;
        let phonemes = phoneme_string(&loaded.g2p.convert(text));
        let mut ids: Vec<i64> = phonemes
            .chars()
            .filter_map(|c| loaded.vocab.get(&c).copied())
            .take(MAX_TOKENS)
            .collect();
        if ids.is_empty() {
            return Ok(Audio {
                samples: Vec::new(),
                sample_rate: SAMPLE_RATE,
            });
        }
        let row = ids.len().min(style_table.len() / STYLE_DIM - 1);
        let style = style_table[row * STYLE_DIM..(row + 1) * STYLE_DIM].to_vec();
        ids.insert(0, 0);
        ids.push(0);
        let input_ids = Tensor::from_array(([1, ids.len()], ids)).map_err(engine_error)?;
        let style = Tensor::from_array(([1, STYLE_DIM], style)).map_err(engine_error)?;
        let speed = Tensor::from_array(([1], vec![rate.clamp(0.5, 2.0)])).map_err(engine_error)?;
        let outputs = loaded
            .session
            .run(inputs!["input_ids" => input_ids, "style" => style, "speed" => speed])
            .map_err(engine_error)?;
        let (_, samples) = outputs["waveform"]
            .try_extract_tensor::<f32>()
            .map_err(engine_error)?;
        Ok(Audio {
            samples: samples.to_vec(),
            sample_rate: SAMPLE_RATE,
        })
    }

    fn is_ready(&self) -> bool {
        self.store.is_installed()
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        KOKORO_VOICES
            .iter()
            .map(|(id, name)| VoiceInfo {
                id: VoiceId::new(*id),
                display_name: (*name).to_owned(),
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "kokoro_tests.rs"]
mod tests;
