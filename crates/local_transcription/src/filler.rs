//! Filler suppression inside the Parakeet TDT decode loop: filler words the decoder emits are
//! dropped once the next word starts, and word-initial filler pieces ("▁uh", "▁um") can get a
//! negative logit bias so the decoder takes blank or the next token instead.
//!
//! A filler is judged as a whole word, so "umbrella", "error", and "ahead" are unaffected, and
//! hyphenated backchannels ("uh-huh", "mm-hmm") are kept because they answer yes or no. Words such
//! as "a" and "I" are never fillers.

use std::collections::HashSet;
use std::ops::Range;

use crate::bias::{BiasMatch, PieceVocab, WORD_START};

/// The default logit penalty on word-initial filler pieces, in nats. Off: on synthesized
/// disfluent speech every penalty raised WER (at 4.0, "error" became "era" and "uh" became "U."),
/// while dropping emitted filler words removed them all with no substitutions.
pub const DEFAULT_FILLER_PENALTY: f32 = 0.0;

/// Fillers in every language Parakeet transcribes.
const UNIVERSAL_FILLERS: &[&str] = &["uh", "uhh", "uhm", "hm", "hmm", "hmmm", "mm", "mmm", "ehm"];
/// Fillers that are also words in other languages ("um" and "er" in German, "eh" in Italian), so
/// they're suppressed only for English.
const ENGLISH_FILLERS: &[&str] = &["um", "umm", "er", "erm", "eh", "ah", "ahh"];

/// How the decoder suppresses fillers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FillerSuppression {
    /// Logit penalty on word-initial filler pieces; 0 disables the bias but still drops emitted
    /// filler words.
    pub penalty: f32,
    /// Whether the speech is English, which adds fillers that are words in other languages.
    pub english: bool,
}

impl FillerSuppression {
    /// Suppression for an ISO 639-1 `language` hint; `None` (auto-detect) uses only the fillers
    /// shared by every language.
    pub fn for_language(language: Option<&str>) -> Self {
        Self {
            penalty: DEFAULT_FILLER_PENALTY,
            english: language.is_some_and(|code| code.eq_ignore_ascii_case("en")),
        }
    }
}

/// [`FillerSuppression`] resolved against a vocabulary.
#[derive(Debug, Clone)]
pub struct FillerFilter {
    penalty: f32,
    english: bool,
    /// Word-initial pieces that are a whole filler word.
    pieces: HashSet<u32>,
}

impl FillerFilter {
    pub fn new(suppression: FillerSuppression, vocab: &PieceVocab) -> Self {
        let mut filter = Self {
            penalty: suppression.penalty.max(0.0),
            english: suppression.english,
            pieces: HashSet::new(),
        };
        filter.pieces = (0..vocab.len() as u32)
            .filter(|id| {
                vocab
                    .piece(*id)
                    .strip_prefix(WORD_START)
                    .is_some_and(|word| filter.is_filler(word))
            })
            .collect();
        filter
    }

    pub fn suppression(&self) -> FillerSuppression {
        FillerSuppression {
            penalty: self.penalty,
            english: self.english,
        }
    }

    /// Whether `word`, without trailing punctuation, is a filler.
    pub fn is_filler(&self, word: &str) -> bool {
        let word = word
            .trim_end_matches([',', '.', '?', '!', ';', ':'])
            .to_lowercase();
        UNIVERSAL_FILLERS.contains(&word.as_str())
            || (self.english && ENGLISH_FILLERS.contains(&word.as_str()))
    }

    /// The logit penalty for emitting `token`.
    pub fn penalty(&self, token: u32) -> f32 {
        if self.pieces.contains(&token) {
            self.penalty
        } else {
            0.0
        }
    }

    /// Picks the token to emit from raw `logits` whose argmax is `best`: the runner-up when the
    /// penalty drops `best` below it.
    pub fn choose(&self, logits: &[f32], best: u32) -> u32 {
        let penalty = self.penalty(best);
        if penalty == 0.0 {
            return best;
        }
        let runner_up = logits
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != best as usize)
            .max_by(|a, b| a.1.total_cmp(b.1));
        match runner_up {
            Some((i, value)) if *value > logits[best as usize] - penalty => i as u32,
            _ => best,
        }
    }

    /// Removes the word at `word` from `tokens` if it is a filler, keeping its sentence-ending
    /// punctuation, and shifts `matches` after it. A word inside a bias match is kept. Returns
    /// whether tokens were removed.
    pub fn drop_filler_word(
        &self,
        tokens: &mut Vec<u32>,
        word: Range<usize>,
        matches: &mut [BiasMatch],
        vocab: &PieceVocab,
    ) -> bool {
        if word.is_empty() || word.end > tokens.len() {
            return false;
        }
        let text: String = tokens[word.clone()]
            .iter()
            .map(|token| vocab.piece(*token).trim_start_matches(WORD_START))
            .collect();
        if !self.is_filler(&text) {
            return false;
        }
        // "called, um, feature": the comma before the filler set it off, so it goes too.
        let has_comma = tokens[word.clone()]
            .iter()
            .any(|token| vocab.piece(*token) == ",");
        let start = if has_comma && word.start > 0 && vocab.piece(tokens[word.start - 1]) == "," {
            word.start - 1
        } else {
            word.start
        };
        if matches.iter().any(|m| m.start < word.end && start < m.end) {
            return false;
        }
        let kept: Vec<u32> = tokens[word.clone()]
            .iter()
            .copied()
            .filter(|token| start > 0 && matches!(vocab.piece(*token), "." | "?" | "!"))
            .collect();
        let removed = word.end - start - kept.len();
        tokens.splice(start..word.end, kept);
        for m in matches.iter_mut().filter(|m| m.start >= word.end) {
            m.start -= removed;
            m.end -= removed;
        }
        removed > 0
    }
}

/// Capitalizes the first letter of `text` and of each sentence in it, for text that lost the
/// capitalized filler that started a sentence.
pub fn capitalize_sentences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at_start = true;
    for c in text.chars() {
        if at_start && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            at_start = false;
            continue;
        }
        if matches!(c, '.' | '?' | '!') {
            at_start = true;
        } else if !c.is_whitespace() {
            at_start = false;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
#[path = "filler_tests.rs"]
mod tests;
