//! Contextual biasing of a transducer decoder toward terms the user is likely to say (file names,
//! identifiers, CLI tools, acronyms on screen), without training. Bias terms are expanded into
//! the ways they are likely to be transcribed, tokenized with the model's SentencePiece vocabulary
//! and stored in a token prefix tree (a "boosting tree", as in NeMo's phrase boosting and
//! TurboBias). While decoding, each token that extends a path in the tree earns a shallow-fusion
//! bonus; leaving the tree before a term completes takes the accumulated bonus back.
//!
//! False alarms are kept down by gating where a path may start: only at a word boundary, only when
//! the model itself gives the first token a reasonable probability, and never against a confident
//! prediction of an everyday English word.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::context_correction::is_common_word;

/// Bias terms kept per request. On synthesized speech, recall barely changes from 200 to 1000
/// terms, while term-free utterances changed by biasing go from under 1% to about 7%.
pub const DEFAULT_BIAS_CAP: usize = 200;
/// Hard limit on the terms a request may carry.
pub const MAX_BIAS_TERMS: usize = 2000;
/// The SentencePiece word-boundary marker.
pub const WORD_START: char = '\u{2581}';

/// A term to bias decoding toward.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BiasTerm {
    /// The term as it is written; transcripts that match one of its spoken forms are rewritten
    /// to this.
    pub text: String,
    /// The agent named the term as one the user is likely to say, so it is already known not to
    /// be an everyday word and starts matching more easily.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trusted: bool,
}

impl BiasTerm {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            trusted: false,
        }
    }

    pub fn trusted(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            trusted: true,
        }
    }
}

/// How strongly decoding is pulled toward bias terms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BiasConfig {
    /// Log-probability bonus per matched token after the first.
    pub weight: f32,
    /// Log-probability bonus for the first token of a term.
    pub entry_weight: f32,
    /// A term may start only where the model gives its first token at least this
    /// log-probability.
    pub entry_min_logprob: f32,
    /// The same for trusted terms.
    pub trusted_entry_min_logprob: f32,
    /// A term never starts against an everyday word the model predicts with at least this
    /// log-probability.
    pub common_word_logprob: f32,
}

impl Default for BiasConfig {
    fn default() -> Self {
        Self {
            weight: 4.0,
            entry_weight: 1.0,
            entry_min_logprob: -4.0,
            trusted_entry_min_logprob: -6.0,
            common_word_logprob: -0.3,
        }
    }
}

/// A SentencePiece vocabulary (`vocab.txt`: one `piece id` per line).
#[derive(Debug, Clone, Default)]
pub struct PieceVocab {
    pieces: Vec<String>,
    /// Each piece lowercased with the word marker as a space, as the tree matches it.
    match_text: Vec<String>,
}

impl PieceVocab {
    pub fn from_vocab_txt(text: &str) -> Self {
        let mut pieces = Vec::new();
        for line in text.lines() {
            let Some((piece, id)) = line.rsplit_once(' ') else {
                continue;
            };
            let Ok(id) = id.trim().parse::<usize>() else {
                continue;
            };
            if id >= pieces.len() {
                pieces.resize(id + 1, String::new());
            }
            pieces[id] = piece.to_owned();
        }
        Self::from_pieces(pieces)
    }

    pub fn from_pieces(pieces: Vec<String>) -> Self {
        let match_text = pieces
            .iter()
            .map(|piece| {
                if piece.starts_with('<') && piece.ends_with('>') {
                    String::new()
                } else {
                    piece.replace(WORD_START, " ").to_lowercase()
                }
            })
            .collect();
        Self { pieces, match_text }
    }

    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    pub fn piece(&self, id: u32) -> &str {
        self.pieces.get(id as usize).map_or("", String::as_str)
    }

    pub fn starts_word(&self, id: u32) -> bool {
        self.piece(id).starts_with(WORD_START)
    }

    fn match_text(&self, id: u32) -> &str {
        self.match_text.get(id as usize).map_or("", String::as_str)
    }
}

/// Surface forms kept per term.
const MAX_FORMS: usize = 24;

/// How people say the start or end of identifier words, so a term also matches its spoken form
/// (`kubectl` heard as "cube control"): (part, spoken forms, whether the part starts the word).
const SPOKEN_PARTS: &[(&str, &[&str], bool)] = &[
    ("kube", &["cube "], true),
    ("py", &["pie ", "pi "], true),
    ("ctl", &[" control", " cuddle", " c t l"], false),
    ("fmt", &[" format", " f m t"], false),
    ("sql", &[" sequel"], false),
];

/// The ways `term` is likely to come out of the recognizer, lowercase (matching ignores case):
/// as written, split into words at `-_./`, all-caps parts spelled letter by letter ("SAST-AI" as
/// "s a s t a i"), and common spoken forms of identifier parts ("kubectl" as "cube control").
pub fn surface_forms(term: &str) -> Vec<String> {
    let term = term.trim();
    let mut forms: Vec<String> = Vec::new();
    let mut push = |form: &str| {
        let form = form
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if !form.is_empty() && !forms.contains(&form) {
            forms.push(form);
        }
    };
    push(term);
    let parts: Vec<&str> = term
        .split(|c: char| matches!(c, '-' | '_' | '.' | '/') || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .collect();
    let is_acronym = |part: &str| {
        let letters = part.chars().filter(|c| c.is_alphabetic()).count();
        (2..=6).contains(&letters)
            && part
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    };
    // Every combination of each word's ways of being said, as written first.
    let mut combinations: Vec<Vec<String>> = vec![Vec::new()];
    for part in &parts {
        let lower = part.to_lowercase();
        let mut alternatives = vec![lower.clone()];
        if is_acronym(part) {
            alternatives.push(
                lower
                    .chars()
                    .map(String::from)
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        // Rules apply to earlier rules' output too, so "kubectl" becomes "cube control".
        for (piece, spoken, at_start) in SPOKEN_PARTS {
            let replaced: Vec<String> = alternatives
                .iter()
                .filter_map(|alt| {
                    if *at_start {
                        alt.strip_prefix(piece).map(|rest| (rest, true))
                    } else {
                        alt.strip_suffix(piece).map(|rest| (rest, false))
                    }
                })
                .flat_map(|(rest, at_start)| {
                    spoken.iter().map(move |said| {
                        if at_start {
                            format!("{said}{rest}")
                        } else {
                            format!("{rest}{said}")
                        }
                    })
                })
                .collect();
            alternatives.extend(replaced);
        }
        combinations = combinations
            .into_iter()
            .flat_map(|prefix| {
                alternatives.iter().map(move |alt| {
                    let mut next = prefix.clone();
                    next.push(alt.clone());
                    next
                })
            })
            .take(MAX_FORMS)
            .collect();
    }
    for words in combinations {
        push(&words.join(" "));
    }
    forms
}

#[derive(Debug, Clone, Default)]
struct Node {
    children: HashMap<char, u32>,
    /// The term a path ending here spells.
    term: Option<u32>,
    /// Some term through this node is trusted.
    trusted: bool,
}

/// A prefix tree of bias terms' surface forms, by character. Matching characters rather than
/// tokens lets a term match however the model splits it into pieces, and ignores case.
#[derive(Debug, Clone)]
pub struct BoostingTree {
    nodes: Vec<Node>,
    terms: Vec<BiasTerm>,
    config: BiasConfig,
}

/// Where a decoding path is in the [`BoostingTree`]; `Copy` so beams can carry it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BiasState {
    node: u32,
    /// Bonus earned since the path entered the tree, taken back if it leaves before a term
    /// completes.
    accumulated: f32,
    /// Index of the token that entered the tree.
    start: usize,
}

impl BiasState {
    /// The state with no bonus to take back. Greedy decoding can't revise tokens it already
    /// emitted, so it scores with this: charging the leave penalty there only keeps the path from
    /// leaving and drops the rest of the utterance into blanks.
    pub fn without_leave_penalty(self) -> Self {
        Self {
            accumulated: 0.0,
            ..self
        }
    }
}

/// A completed match: tokens `start..end` of the hypothesis spell [`BoostingTree::term`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BiasMatch {
    pub start: usize,
    pub end: usize,
    pub term: u32,
}

impl BoostingTree {
    pub fn build(terms: &[BiasTerm], config: BiasConfig) -> Self {
        let mut tree = Self {
            nodes: vec![Node::default()],
            terms: Vec::new(),
            config,
        };
        let mut seen = HashSet::new();
        for term in terms.iter().take(MAX_BIAS_TERMS) {
            if !seen.insert(term.text.to_lowercase()) {
                continue;
            }
            let index = tree.terms.len() as u32;
            for form in surface_forms(&term.text) {
                tree.insert(&format!(" {form}"), index, term.trusted);
            }
            tree.terms.push(term.clone());
        }
        tree
    }

    fn insert(&mut self, form: &str, term: u32, trusted: bool) {
        let mut node = 0usize;
        for c in form.chars() {
            node = match self.nodes[node].children.get(&c) {
                Some(&next) => next as usize,
                None => {
                    self.nodes.push(Node::default());
                    let next = self.nodes.len() - 1;
                    self.nodes[node].children.insert(c, next as u32);
                    next
                }
            };
            self.nodes[node].trusted |= trusted;
        }
        if self.nodes[node].term.is_none() {
            self.nodes[node].term = Some(term);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn term_count(&self) -> usize {
        self.terms.len()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn term(&self, index: u32) -> &BiasTerm {
        &self.terms[index as usize]
    }

    /// The node reached by reading `text` from `node`.
    fn walk(&self, mut node: u32, text: &str) -> Option<u32> {
        for c in text.chars() {
            node = *self.nodes[node as usize].children.get(&c)?;
        }
        Some(node)
    }

    /// Where `token` leads from `state`: further along its path, or (at a word boundary) onto a
    /// new path from the root. The flag is whether it starts a new path.
    fn step(&self, state: &BiasState, token: u32, vocab: &PieceVocab) -> Option<(u32, bool)> {
        let text = vocab.match_text(token);
        if text.is_empty() {
            return None;
        }
        if state.node != 0
            && let Some(next) = self.walk(state.node, text)
        {
            return Some((next, false));
        }
        if !text.starts_with(' ') {
            return None;
        }
        self.walk(0, text).map(|next| (next, true))
    }

    /// The score change of emitting `token` from `state`, given the model's log-probability of
    /// that token and of its unbiased best token, or `None` if `token` doesn't touch the tree
    /// (its change is then minus [`Self::leave_penalty`]).
    pub fn bonus(
        &self,
        state: &BiasState,
        token: u32,
        logprob: f32,
        best: (u32, f32),
        vocab: &PieceVocab,
    ) -> Option<f32> {
        let (next, enters) = self.step(state, token, vocab)?;
        if !enters {
            return Some(self.config.weight);
        }
        let min_logprob = if self.nodes[next as usize].trusted {
            self.config.trusted_entry_min_logprob
        } else {
            self.config.entry_min_logprob
        };
        let (best_token, best_logprob) = best;
        let replaces_common_word = best_token != token
            && best_logprob >= self.config.common_word_logprob
            && is_common_word(vocab.match_text(best_token).trim());
        if logprob < min_logprob || replaces_common_word {
            return None;
        }
        Some(self.config.entry_weight - self.leave_penalty(state))
    }

    /// What leaving the tree from `state` costs: the bonus earned on an unfinished path.
    pub fn leave_penalty(&self, state: &BiasState) -> f32 {
        state.accumulated
    }

    /// The state after emitting `token` at hypothesis position `position`, and the match it
    /// completes, if any. `bonus` is what [`Self::bonus`] returned for it.
    pub fn advance(
        &self,
        state: &BiasState,
        token: u32,
        position: usize,
        bonus: Option<f32>,
        vocab: &PieceVocab,
    ) -> (BiasState, Option<BiasMatch>) {
        let (Some(bonus), Some((next, enters))) = (bonus, self.step(state, token, vocab)) else {
            return (BiasState::default(), None);
        };
        let (start, accumulated) = if enters {
            (position, bonus + self.leave_penalty(state))
        } else {
            (state.start, state.accumulated + bonus)
        };
        let node = &self.nodes[next as usize];
        let found = node.term.map(|term| BiasMatch {
            start,
            end: position + 1,
            term,
        });
        if found.is_some() && node.children.is_empty() {
            return (BiasState::default(), found);
        }
        // A completed term keeps its bonus even if the path goes on and then leaves.
        let accumulated = if found.is_some() { 0.0 } else { accumulated };
        (
            BiasState {
                node: next,
                accumulated,
                start,
            },
            found,
        )
    }
}

/// Builds the text of `tokens`, writing each match in `matches` as its term is written.
/// Overlapping matches keep the longest.
pub fn render_with_matches(
    tokens: &[u32],
    matches: &[BiasMatch],
    tree: &BoostingTree,
    vocab: &PieceVocab,
) -> String {
    let mut chosen: Vec<BiasMatch> = Vec::new();
    let mut sorted = matches.to_vec();
    sorted.sort_by_key(|m| (m.start, std::cmp::Reverse(m.end)));
    for m in sorted {
        match chosen.last_mut() {
            Some(last) if m.start < last.end => {
                if m.end - m.start > last.end - last.start {
                    *last = m;
                }
            }
            _ => chosen.push(m),
        }
    }
    let mut text = String::new();
    let mut index = 0;
    let mut matches = chosen.iter().peekable();
    while index < tokens.len() {
        if let Some(m) = matches.next_if(|m| m.start == index) {
            if vocab.starts_word(tokens[index]) {
                text.push(' ');
            }
            text.push_str(&tree.term(m.term).text);
            index = m.end;
            continue;
        }
        push_piece(&mut text, vocab.piece(tokens[index]));
        index += 1;
    }
    text
}

/// The text of `tokens`.
pub fn detokenize(tokens: &[u32], vocab: &PieceVocab) -> String {
    let mut text = String::new();
    for &token in tokens {
        push_piece(&mut text, vocab.piece(token));
    }
    text
}

/// Appends one piece's display text. Digits carry no word marker, so one after a word gets a
/// space ("at 60", not "at60"), except after a single capital ("A4").
fn push_piece(text: &mut String, piece: &str) {
    if piece.starts_with('<') && piece.ends_with('>') && piece != "<unk>" {
        return;
    }
    let display = piece.replace(WORD_START, " ");
    if !text.is_empty() && !display.starts_with(' ') && display.chars().all(|c| c.is_ascii_digit())
    {
        let trailing_letters = text.chars().rev().take_while(|c| c.is_alphabetic()).count();
        let article_a = trailing_letters == 1 && text.ends_with('a');
        if trailing_letters > 1 || article_a {
            text.push(' ');
        }
    }
    text.push_str(&display);
}

/// Words in prose that name something rather than being ordinary English: identifiers and paths
/// (with `-_./`), mixed case (`OpenAI`), acronyms (`SAST`), and capitalized words that don't start a
/// sentence (`Cerebras`). File paths contribute their base name.
pub fn salient_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut sentence_start = true;
    for raw in text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '`' | '"' | '(' | ')' | ',' | ';' | '*' | '[' | ']' | '<' | '>' | '|'
            )
    }) {
        if raw.is_empty() {
            continue;
        }
        let ends_sentence = raw.ends_with(['.', '!', '?', ':']);
        let word = raw.trim_matches(|c: char| matches!(c, '.' | ':' | '!' | '?' | '\'' | '#'));
        let starts_sentence = sentence_start;
        sentence_start = ends_sentence;
        if word.contains("://")
            || word.chars().filter(|c| c.is_alphabetic()).count() < 2
            || !word
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '@' | '~'))
        {
            continue;
        }
        let word = match word.rsplit_once('/') {
            Some((_, base)) if !base.is_empty() => base,
            _ => word,
        };
        let symbol = word.contains(['-', '_', '.']);
        let letters: Vec<char> = word.chars().filter(|c| c.is_alphabetic()).collect();
        let mixed = letters.iter().skip(1).any(|c| c.is_uppercase())
            && letters.iter().any(|c| c.is_lowercase());
        let acronym = letters.len() >= 2 && letters.iter().all(|c| c.is_uppercase());
        let capitalized = !starts_sentence && letters.first().is_some_and(|c| c.is_uppercase());
        if (symbol || mixed || acronym || capitalized) && !is_common_word(word) {
            tokens.push(word.to_owned());
        }
    }
    tokens
}

/// A candidate bias term with its ranking score.
#[derive(Debug, Clone, PartialEq)]
pub struct BiasCandidate {
    pub text: String,
    pub score: f32,
    pub trusted: bool,
}

/// Whether `term` is worth biasing toward: it has letters, isn't an everyday word or phrase of
/// them, and is short enough to be said in one breath.
pub fn is_bias_worthy(term: &str) -> bool {
    let term = term.trim();
    let words: Vec<&str> = term.split_whitespace().collect();
    term.chars().filter(|c| c.is_alphanumeric()).count() >= 3
        && term.chars().any(char::is_alphabetic)
        && term.len() <= 48
        && (1..=4).contains(&words.len())
        && !term.contains("://")
        && words.iter().any(|word| !is_common_word(word))
}

/// Ranks candidates by score (deduplicated case-insensitively, keeping the best score and any
/// trust) and keeps the best `cap` worth biasing toward.
pub fn rank_bias_terms(
    candidates: impl IntoIterator<Item = BiasCandidate>,
    cap: usize,
) -> Vec<BiasTerm> {
    let mut best: HashMap<String, BiasCandidate> = HashMap::new();
    for candidate in candidates {
        // Trusted terms may be as short as an acronym like "CI".
        let worthy = if candidate.trusted {
            candidate
                .text
                .chars()
                .filter(|c| c.is_alphanumeric())
                .count()
                >= 2
                && candidate
                    .text
                    .split_whitespace()
                    .any(|word| !is_common_word(word))
        } else {
            is_bias_worthy(&candidate.text)
        };
        if !worthy {
            continue;
        }
        let text = candidate.text.trim();
        if text.is_empty() || text.len() > 48 {
            continue;
        }
        best.entry(text.to_lowercase())
            .and_modify(|existing| {
                existing.trusted |= candidate.trusted;
                if candidate.score > existing.score {
                    existing.score = candidate.score;
                    existing.text = text.to_owned();
                }
            })
            .or_insert_with(|| BiasCandidate {
                text: text.to_owned(),
                ..candidate
            });
    }
    let mut ranked: Vec<BiasCandidate> = best.into_values().collect();
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.text.cmp(&b.text))
    });
    ranked
        .into_iter()
        .take(cap.min(MAX_BIAS_TERMS))
        .map(|candidate| BiasTerm {
            text: candidate.text,
            trusted: candidate.trusted,
        })
        .collect()
}

#[cfg(test)]
#[path = "bias_tests.rs"]
mod tests;
