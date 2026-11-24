use std::sync::Mutex;

use parakeet_rs::ort::inputs;
use parakeet_rs::ort::session::Session;
use parakeet_rs::ort::value::Tensor;

use crate::TranscribeError;
use crate::disfluency::{DisfluencyScorer, WordPiece};

/// The int8 BERT-mini tagger's ONNX graph, bundled with the app because it is small.
const MODEL: &[u8] = include_bytes!("../models/disfluency/disfluency-bert-mini.int8.onnx");
const VOCAB: &str = include_str!("../models/disfluency/vocab.txt");
/// Longest piece sequence one model call takes, including `[CLS]` and `[SEP]`; longer utterances
/// are scored in word-aligned windows.
const MAX_PIECES: usize = 128;

fn engine_error(error: impl std::fmt::Display) -> TranscribeError {
    TranscribeError::Engine(error.to_string())
}

/// A BERT-mini token classifier (Apache-2.0 base, fine-tuned on synthetic disfluencies over
/// permissively licensed text) on ONNX Runtime (CPU) that scores each word of a transcript.
pub struct DisfluencyTagger {
    session: Mutex<Session>,
    tokenizer: WordPiece,
}

impl DisfluencyTagger {
    pub fn load() -> Result<Self, TranscribeError> {
        let tokenizer = WordPiece::from_vocab_txt(VOCAB)?;
        let environment = crate::ort_env::environment()?;
        let session = Session::builder(&environment)
            .and_then(|mut builder| {
                builder = builder.with_intra_threads(1)?.with_memory_pattern(false)?;
                builder.commit_from_memory(MODEL)
            })
            .map_err(engine_error)?;
        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
        })
    }

    /// Scores `words` in one model call; each word's score is its first piece's.
    fn score_window(
        &self,
        words: &[Vec<i64>],
        session: &mut Session,
    ) -> Result<Vec<f32>, TranscribeError> {
        let mut ids = vec![self.tokenizer.cls()];
        let mut first_piece = Vec::with_capacity(words.len());
        for pieces in words {
            first_piece.push(ids.len());
            ids.extend(pieces);
        }
        ids.push(self.tokenizer.sep());
        let len = ids.len();
        let tensor =
            |values: Vec<i64>| Tensor::from_array(([1, len], values)).map_err(engine_error);
        let outputs = session
            .run(inputs![
                "input_ids" => tensor(ids)?,
                "attention_mask" => tensor(vec![1; len])?,
                "token_type_ids" => tensor(vec![0; len])?,
            ])
            .map_err(engine_error)?;
        let (_, logits) = outputs["logits"]
            .try_extract_tensor::<f32>()
            .map_err(engine_error)?;
        Ok(first_piece
            .into_iter()
            .map(|at| {
                let (keep, delete) = (logits[2 * at], logits[2 * at + 1]);
                1.0 / (1.0 + (keep - delete).exp())
            })
            .collect())
    }
}

impl DisfluencyScorer for DisfluencyTagger {
    fn delete_probabilities(&self, words: &[&str]) -> Result<Vec<f32>, TranscribeError> {
        let pieces: Vec<Vec<i64>> = words
            .iter()
            .map(|word| {
                let mut ids = self.tokenizer.word_ids(word);
                ids.truncate(MAX_PIECES - 2);
                ids
            })
            .collect();
        let mut session = self
            .session
            .lock()
            .map_err(|_| TranscribeError::Engine("the disfluency model lock is poisoned".into()))?;
        let mut scores = Vec::with_capacity(words.len());
        let mut start = 0;
        while start < pieces.len() {
            let mut end = start;
            let mut total = 2;
            while end < pieces.len() && total + pieces[end].len() <= MAX_PIECES {
                total += pieces[end].len();
                end += 1;
            }
            scores.extend(self.score_window(&pieces[start..end], &mut session)?);
            start = end;
        }
        Ok(scores)
    }
}
