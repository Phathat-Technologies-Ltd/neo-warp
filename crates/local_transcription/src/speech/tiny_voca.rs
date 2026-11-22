use std::collections::HashMap;
use std::sync::Mutex;

use parakeet_rs::ort::inputs;
use parakeet_rs::ort::session::Session;
use parakeet_rs::ort::value::Tensor;

use super::g2p::{G2p, G2pToken, Phone};
use super::{SpeakError, Speaker, VoiceId, VoiceInfo};
use crate::{Audio, ModelStore};

pub const TINY_VOCA_MODEL_FILE: &str = "model.onnx";
pub const TINY_VOCA_CONFIG_FILE: &str = "config.json";
const SAMPLE_RATE: u32 = 24_000;
const MAX_TOKENS: usize = 512;

/// The voices of [`crate::tiny_voca`], the default first, as (id, name). Their index is the
/// model's `voice` input.
pub const TINY_VOCA_VOICES: &[(&str, &str)] = &[("F2", "F2 (US female)"), ("M2", "M2 (US male)")];

/// tiny-voca (Apache-2.0), a 4.5M-parameter ONNX model, on ONNX Runtime (CPU).
pub struct TinyVocaSpeaker {
    store: ModelStore,
    loaded: Mutex<Option<Loaded>>,
}

struct Loaded {
    session: Session,
    vocab: HashMap<String, i64>,
    g2p: G2p,
}

fn engine_error(error: impl std::fmt::Display) -> SpeakError {
    SpeakError::Engine(error.to_string())
}

impl TinyVocaSpeaker {
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
                builder.commit_from_file(dir.join(TINY_VOCA_MODEL_FILE))
            })
            .map_err(engine_error)?;
        let config =
            std::fs::read_to_string(dir.join(TINY_VOCA_CONFIG_FILE)).map_err(engine_error)?;
        Ok(Loaded {
            session,
            vocab: parse_vocab(&config)?,
            g2p: G2p::load(dir)?,
        })
    }
}

fn parse_vocab(config: &str) -> Result<HashMap<String, i64>, SpeakError> {
    let json: serde_json::Value = serde_json::from_str(config).map_err(engine_error)?;
    let vocab = json
        .get("vocab")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| SpeakError::Engine("config.json has no vocab".to_owned()))?;
    Ok(vocab
        .iter()
        .filter_map(|(symbol, id)| Some((symbol.clone(), id.as_i64()?)))
        .collect())
}

/// Maps an ARPAbet phone to tiny-voca's phoneme symbols, given the phone after it so r-colored
/// vowels can merge.
fn voca_symbol(phone: &Phone, next: Option<&Phone>) -> (&'static str, bool) {
    let unstressed = phone.stress == Some(0);
    let before_r = next.is_some_and(|next| next.symbol == "R");
    let merged = |symbol| (symbol, true);
    match phone.symbol.as_str() {
        "AA" if before_r => merged("ɑːɹ"),
        "AO" if before_r => merged("ɔːɹ"),
        "IH" | "IY" if before_r => merged("ɪɹ"),
        "EH" if before_r => merged("ɛɹ"),
        "UH" if before_r => merged("ʊɹ"),
        "AA" => ("ɑː", false),
        "AE" => ("æ", false),
        "AH" if unstressed => ("ə", false),
        "AH" => ("ʌ", false),
        "AO" => ("ɔː", false),
        "AW" => ("aʊ", false),
        "AY" => ("aɪ", false),
        "EH" => ("ɛ", false),
        "ER" if unstressed => ("ɚ", false),
        "ER" => ("ɜː", false),
        "EY" => ("eɪ", false),
        "IH" => ("ɪ", false),
        "IY" if unstressed => ("i", false),
        "IY" => ("iː", false),
        "OW" => ("oʊ", false),
        "OY" => ("ɔɪ", false),
        "UH" => ("ʊ", false),
        "UW" => ("uː", false),
        "B" => ("b", false),
        "CH" => ("tʃ", false),
        "D" => ("d", false),
        "DH" => ("ð", false),
        "F" => ("f", false),
        "G" => ("ɡ", false),
        "HH" => ("h", false),
        "JH" => ("dʒ", false),
        "K" => ("k", false),
        "L" => ("l", false),
        "M" => ("m", false),
        "N" => ("n", false),
        "NG" => ("ŋ", false),
        "P" => ("p", false),
        "R" => ("ɹ", false),
        "S" => ("s", false),
        "SH" => ("ʃ", false),
        "T" => ("t", false),
        "TH" => ("θ", false),
        "V" => ("v", false),
        "W" => ("w", false),
        "Y" => ("j", false),
        "Z" => ("z", false),
        "ZH" => ("ʒ", false),
        _ => ("<unk>", false),
    }
}

/// Builds the model's token and stress sequences, framed by `<bos>` and `<eos>`.
pub(crate) fn encode(tokens: &[G2pToken], vocab: &HashMap<String, i64>) -> (Vec<i64>, Vec<i64>) {
    let id = |symbol: &str| vocab.get(symbol).or_else(|| vocab.get("<unk>")).copied();
    let mut ids = Vec::new();
    let mut stress = Vec::new();
    let mut push = |symbol: &str, value: i64| {
        if let Some(token) = id(symbol) {
            ids.push(token);
            stress.push(value);
        }
    };
    push("<bos>", 0);
    let mut ended_with_mark = false;
    for (index, token) in tokens.iter().enumerate() {
        match token {
            G2pToken::Word(phones) => {
                if index > 0 && !ended_with_mark {
                    push("<sp>", 0);
                }
                let mut i = 0;
                while i < phones.len() {
                    let (symbol, merged) = voca_symbol(&phones[i], phones.get(i + 1));
                    push(symbol, i64::from(phones[i].stress.unwrap_or(0)));
                    i += if merged { 2 } else { 1 };
                }
                ended_with_mark = false;
            }
            G2pToken::Punct(mark) => {
                push(&mark.to_string(), 0);
                push("<sp>", 0);
                ended_with_mark = true;
            }
        }
    }
    // Without a final mark the last word breaks off.
    if !matches!(tokens.last(), Some(G2pToken::Punct(_))) {
        push(".", 0);
    }
    push("<eos>", 0);
    ids.truncate(MAX_TOKENS);
    stress.truncate(MAX_TOKENS);
    (ids, stress)
}

impl Speaker for TinyVocaSpeaker {
    fn synthesize(&self, text: &str, voice: &VoiceId, rate: f32) -> Result<Audio, SpeakError> {
        if !self.store.is_installed() {
            return Err(SpeakError::ModelNotInstalled);
        }
        let voice_index = TINY_VOCA_VOICES
            .iter()
            .position(|(id, _)| *id == voice.as_str())
            .ok_or_else(|| SpeakError::UnknownVoice(voice.as_str().to_owned()))?;
        let mut loaded = self
            .loaded
            .lock()
            .map_err(|_| SpeakError::Engine("the tiny-voca lock is poisoned".to_owned()))?;
        if loaded.is_none() {
            *loaded = Some(self.load()?);
        }
        let loaded = loaded.as_mut().expect("loaded above");
        let (ids, stress) = encode(&loaded.g2p.convert(text), &loaded.vocab);
        let len = ids.len();
        let one = |value: f32| Tensor::from_array(([1], vec![value])).map_err(engine_error);
        let outputs = loaded
            .session
            .run(inputs![
                "tokens" => Tensor::from_array(([1, len], ids)).map_err(engine_error)?,
                "stress" => Tensor::from_array(([1, len], stress)).map_err(engine_error)?,
                "voice" => Tensor::from_array(([1], vec![voice_index as i64])).map_err(engine_error)?,
                "speed" => one(rate.clamp(0.5, 2.0))?,
                "pitch_shift" => one(0.0)?,
                "pitch_range" => one(1.0)?,
            ])
            .map_err(engine_error)?;
        let (_, samples) = outputs["audio"]
            .try_extract_tensor::<f32>()
            .map_err(engine_error)?;
        // The model can peak a little above 1.0.
        let peak = samples.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
        let scale = if peak > 0.99 { 0.99 / peak } else { 1.0 };
        Ok(Audio {
            samples: samples.iter().map(|s| s * scale).collect(),
            sample_rate: SAMPLE_RATE,
        })
    }

    fn is_ready(&self) -> bool {
        self.store.is_installed()
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        TINY_VOCA_VOICES
            .iter()
            .map(|(id, name)| VoiceInfo {
                id: VoiceId::new(*id),
                display_name: (*name).to_owned(),
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "tiny_voca_tests.rs"]
mod tests;
