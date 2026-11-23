//! Screen-aware correction of transcripts: spans of one to four spoken words are matched against
//! a [`Vocabulary`] of terms visible to the user (file names, branches, commands) by spelling and
//! by a phonetic key, and replaced when the match is strong. A [`CorrectionLog`] remembers what
//! was corrected so user-pinned mappings apply first and reverted ones stop applying. Everything
//! is plain lookups; there is no model and no training.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// Default cap on the number of vocabulary terms.
pub const MAX_VOCABULARY_TERMS: usize = 2000;
/// Longest spoken span, in words, matched against one term.
const MAX_SPAN_WORDS: usize = 4;
/// Minimum confidence for replacing a span.
pub const DEFAULT_THRESHOLD: f32 = 0.88;
/// Single words are more often ordinary speech, so they need a stronger match and a longer key.
const SINGLE_WORD_THRESHOLD: f32 = 0.95;
const SINGLE_WORD_MIN_KEY: usize = 4;
/// Reverts that suppress a learned mapping once they outnumber its accepts.
const MIN_REVERTS_TO_SUPPRESS: u32 = 1;

/// The spoken span and the term must also be this similar letter for letter, so a term never
/// swallows extra spoken words ("can you" is never replaced by `can`).
const MIN_SPELLING_SIMILARITY: f32 = 0.6;

/// Everyday English words, including command names that double as words. A term made only of
/// these is never a replacement, and a single one of these is never replaced.
const COMMON_WORDS: &[&str] = &[
    "a", "about", "add", "after", "again", "all", "also", "am", "an", "and", "any", "are", "as",
    "ask", "at", "back", "be", "because", "been", "before", "build", "but", "by", "call", "can",
    "cat", "change", "check", "clean", "clear", "close", "code", "come", "copy", "could",
    "current", "cut", "date", "delete", "did", "diff", "do", "does", "done", "down", "each",
    "echo", "edit", "else", "error", "even", "file", "files", "find", "first", "fix", "folder",
    "for", "from", "get", "give", "go", "good", "got", "had", "has", "have", "he", "head", "hello",
    "help", "her", "here", "hey", "hi", "him", "his", "hour", "how", "i", "if", "in", "into", "is",
    "it", "its", "just", "kill", "know", "last", "less", "let", "like", "line", "link", "list",
    "log", "logs", "look", "main", "make", "man", "may", "me", "more", "most", "move", "my",
    "name", "need", "new", "next", "no", "not", "now", "of", "off", "ok", "okay", "on", "one",
    "only", "open", "or", "other", "our", "out", "over", "please", "print", "pull", "push", "put",
    "read", "remove", "rename", "reset", "right", "run", "say", "see", "set", "she", "should",
    "show", "so", "some", "sort", "start", "status", "stop", "take", "tell", "test", "thanks",
    "that", "the", "their", "them", "then", "there", "these", "they", "this", "time", "to", "top",
    "touch", "try", "two", "up", "us", "use", "want", "was", "watch", "way", "we", "well", "were",
    "what", "when", "where", "which", "who", "why", "will", "with", "would", "write", "yes", "you",
    "your",
];

/// Where a transcript goes, which sets how freely it is corrected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionContext {
    /// Shell input: commands, file names, and identifiers are expected.
    Command,
    /// A natural-language prompt: only multi-word spans that clearly name a file, directory, or
    /// identifier (a term with a symbol, digit, or inner capital) are corrected.
    Prose,
}

fn is_identifier_shaped(term: &str) -> bool {
    term.chars()
        .any(|c| c.is_ascii_digit() || matches!(c, '.' | '/' | '_' | '-' | '~' | '@'))
        || term
            .split_whitespace()
            .any(|word| word.chars().skip(1).any(char::is_uppercase))
}

pub(crate) fn is_common_word(word: &str) -> bool {
    COMMON_WORDS.contains(&word.to_lowercase().as_str())
}

/// Whether `term` is vocabulary rather than ordinary speech: it has a symbol, digit, or inner
/// capital, or at least one word that isn't [`COMMON_WORDS`].
fn is_vocabulary_term(term: &str) -> bool {
    is_identifier_shaped(term) || term.split_whitespace().any(|word| !is_common_word(word))
}

/// Spoken names of the symbols that appear inside terms.
const SYMBOL_WORDS: &[(char, &str)] = &[
    ('.', "dot"),
    ('/', "slash"),
    ('_', "underscore"),
    ('-', "dash"),
    ('~', "tilde"),
    ('@', "at"),
];

/// Abbreviations people expand when they say a term aloud (`kubectl` as "cube control").
const SPOKEN_ABBREVIATIONS: &[(&str, &str)] = &[
    ("ctl", "control"),
    ("cfg", "config"),
    ("conf", "config"),
    ("dir", "directory"),
    ("src", "source"),
    ("py", "pie"),
];

#[derive(Debug, Clone)]
struct Term {
    text: String,
    /// Letter-only spellings of the ways the term can be said.
    spellings: Vec<String>,
    /// Phonetic keys of those spellings.
    keys: Vec<String>,
}

/// Terms a transcript may be corrected towards, deduplicated and capped.
#[derive(Debug, Clone)]
pub struct Vocabulary {
    terms: Vec<Term>,
    seen: HashSet<String>,
    cap: usize,
}

impl Default for Vocabulary {
    fn default() -> Self {
        Self::with_cap(MAX_VOCABULARY_TERMS)
    }
}

impl Vocabulary {
    pub fn with_cap(cap: usize) -> Self {
        Self {
            terms: Vec::new(),
            seen: HashSet::new(),
            cap,
        }
    }

    pub fn len(&self) -> usize {
        self.terms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn contains(&self, term: &str) -> bool {
        self.seen.contains(term)
    }

    /// Adds `term` unless it is a duplicate, unsuitable (too short, too long, no letters), or the
    /// vocabulary is full. Returns whether it was added.
    pub fn add(&mut self, term: &str) -> bool {
        let term = term.trim();
        if self.terms.len() >= self.cap
            || term.chars().count() < 2
            || term.starts_with('-')
            || term.len() > 64
            || !term.chars().any(char::is_alphabetic)
            || term.chars().any(char::is_whitespace) && term.split_whitespace().count() > 3
            || self.seen.contains(term)
        {
            return false;
        }
        let spellings = spellings_of(term);
        let keys = spellings.iter().map(|s| phonetic_key(s)).collect();
        self.seen.insert(term.to_owned());
        self.terms.push(Term {
            text: term.to_owned(),
            spellings,
            keys,
        });
        true
    }

    pub fn extend<'a>(&mut self, terms: impl IntoIterator<Item = &'a str>) {
        for term in terms {
            if self.terms.len() >= self.cap {
                break;
            }
            self.add(term);
        }
    }
}

/// Words in `text` that look like paths or identifiers (`main.rs`, `atm-design-analyzer`,
/// `src/lib`, `fooBar`), for building a vocabulary from terminal output or an agent reply.
pub fn identifier_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '`' | '"' | '\'' | '(' | ')' | ',' | ';'))
        .map(|word| word.trim_matches(|c: char| matches!(c, '.' | ':' | '!' | '?' | '[' | ']')))
        .filter(|word| {
            let symbol = word.contains(['-', '_', '.', '/']);
            let camel = word.chars().skip(1).any(char::is_uppercase)
                && word.chars().any(char::is_lowercase);
            (symbol || camel)
                && word.len() >= 3
                && word.chars().any(char::is_alphabetic)
                && !word.contains("://")
                && word
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '~' | '@'))
        })
}

/// The letter-only spellings of the ways `term` can be said: symbols silent ("atmdesignanalyzer"),
/// symbols spoken ("pyprojectdottoml"), and abbreviations expanded ("kubecontrol").
fn spellings_of(term: &str) -> Vec<String> {
    let lower = term.to_lowercase();
    let silent: String = lower.chars().filter(|c| c.is_alphanumeric()).collect();
    let spoken: String = lower
        .chars()
        .map(|c| {
            SYMBOL_WORDS
                .iter()
                .find(|(symbol, _)| *symbol == c)
                .map_or_else(|| c.to_string(), |(_, word)| (*word).to_owned())
        })
        .filter(|s| s.chars().all(char::is_alphanumeric))
        .collect();
    let mut spellings = vec![silent.clone()];
    for base in [&silent, &spoken] {
        if !spellings.contains(base) {
            spellings.push(base.clone());
        }
        for (short, long) in SPOKEN_ABBREVIATIONS {
            if base.contains(short) {
                let expanded = base.replace(short, long);
                if !spellings.contains(&expanded) {
                    spellings.push(expanded);
                }
            }
        }
    }
    spellings.retain(|s| !s.is_empty());
    spellings
}

/// A simple phonetic key: similar-sounding consonants merge, vowels after the first letter drop,
/// and repeats collapse, so "pie project" and "pyproject" share the key `prjkt`.
pub fn phonetic_key(word: &str) -> String {
    let letters: Vec<char> = word
        .to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let mut out = String::new();
    let mut last = None;
    let mut index = 0;
    while index < letters.len() {
        let c = letters[index];
        let next = letters.get(index + 1).copied();
        let (code, skip) = match (c, next) {
            ('p', Some('h')) => (Some('f'), 2),
            ('c', Some('k')) => (Some('k'), 2),
            ('s', Some('h')) | ('c', Some('h')) => (Some('x'), 2),
            ('t', Some('h')) => (Some('0'), 2),
            ('g', Some('h')) => (None, 2),
            ('d', Some('g')) => (Some('j'), 2),
            ('c', Some('e' | 'i' | 'y')) => (Some('s'), 1),
            ('c' | 'q' | 'k', _) => (Some('k'), 1),
            ('g', Some('e' | 'i')) if index > 0 => (Some('j'), 1),
            ('x', _) => (Some('k'), 1),
            ('z', _) => (Some('s'), 1),
            ('v', _) => (Some('f'), 1),
            ('b' | 'p', _) => (Some(c), 1),
            ('a' | 'e' | 'i' | 'o' | 'u' | 'y' | 'w' | 'h', _) => {
                (if index == 0 { Some('a') } else { None }, 1)
            }
            _ => (Some(c), 1),
        };
        if let Some(code) = code {
            if last != Some(code) {
                out.push(code);
            }
            last = Some(code);
        } else if !matches!(c, 'h' | 'w') {
            // A vowel separates repeated consonants ("dot toml" keeps one `t`, "data" keeps both).
            last = None;
        }
        index += skip;
    }
    // Dropping vowels makes "dot toml" and "dottoml" differ only in a collapsed repeat.
    let mut deduped = String::new();
    for c in out.chars() {
        if !deduped.ends_with(c) {
            deduped.push(c);
        }
    }
    deduped
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn similarity(a: &str, b: &str) -> f32 {
    let longest = a.chars().count().max(b.chars().count());
    if longest == 0 {
        return 0.;
    }
    1. - levenshtein(a, b) as f32 / longest as f32
}

/// How confidently the spoken `spelling` (letters only) and its key refer to `term`.
fn confidence(spelling: &str, key: &str, term: &Term) -> f32 {
    term.spellings
        .iter()
        .zip(&term.keys)
        .map(|(term_spelling, term_key)| {
            let by_spelling = similarity(spelling, term_spelling);
            // Keys are short, so require them to match exactly or nearly, and let the spelling
            // break ties between terms with the same key.
            let by_sound = if key.len() >= 2 && key == term_key {
                0.9 + 0.1 * by_spelling
            } else if key.len() >= 5 && levenshtein(key, term_key) == 1 {
                0.8 + 0.1 * by_spelling
            } else {
                0.
            };
            by_spelling.max(by_sound)
        })
        .fold(0., f32::max)
}

/// One replacement made in a transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct Correction {
    /// The words as transcribed.
    pub heard: String,
    /// The term that replaced them.
    pub corrected: String,
    /// Byte range of `corrected` in the corrected text.
    pub range: std::ops::Range<usize>,
    pub confidence: f32,
    /// Whether a user-pinned mapping made the replacement.
    pub pinned: bool,
}

/// The result of [`correct`].
#[derive(Debug, Clone, PartialEq)]
pub struct Corrected {
    pub text: String,
    pub corrections: Vec<Correction>,
}

/// Normalizes words to a lookup key for the correction log: lowercase, sentence punctuation
/// stripped, single spaces.
pub fn heard_key(words: &str) -> String {
    words
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| matches!(c, ',' | '.' | '?' | '!' | ';' | '"'))
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Trailing sentence punctuation of `word`, which a replacement keeps.
fn trailing_punctuation(word: &str) -> &str {
    let trimmed = word.trim_end_matches([',', '.', '?', '!', ';']);
    &word[trimmed.len()..]
}

/// Rewrites spans of `text` that match `vocabulary` or a pinned mapping in `log`. Pinned
/// mappings apply first; mappings the user reverted or disabled are skipped; matches below
/// `threshold` are left alone. `context` limits what may be corrected; see
/// [`CorrectionContext`].
pub fn correct(
    text: &str,
    vocabulary: &Vocabulary,
    log: &CorrectionLog,
    threshold: f32,
    context: CorrectionContext,
) -> Corrected {
    let words: Vec<&str> = text.split_whitespace().collect();
    let keys: Vec<String> = words.iter().map(|w| heard_key(w)).collect();
    let mut out = String::new();
    let mut corrections = Vec::new();
    let mut index = 0;
    while index < words.len() {
        let mut best: Option<(usize, String, f32, bool)> = None;
        let longest = MAX_SPAN_WORDS.min(words.len() - index);
        // A span that already spells a term (as biased decoding writes them) is kept, so the
        // two never fight over it.
        let exact = (1..=longest).rev().find(|&len| {
            len > 1
                && vocabulary.contains(
                    words[index..index + len]
                        .join(" ")
                        .trim_end_matches([',', '.', '?', '!', ';']),
                )
        });
        if let Some(len) = exact {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&words[index..index + len].join(" "));
            index += len;
            continue;
        }
        for len in (1..=longest).rev() {
            let heard = keys[index..index + len].join(" ");
            if heard.is_empty() {
                continue;
            }
            if let Some(target) = log.pinned_target(&heard) {
                best = Some((len, target.to_owned(), 1., true));
                break;
            }
        }
        if best.is_none() {
            // Every span length competes; on a tie the longer span, tried first, wins.
            for len in (1..=longest).rev() {
                let span = &keys[index..index + len];
                let heard = span.join(" ");
                let spelling: String = heard.chars().filter(|c| c.is_alphanumeric()).collect();
                if spelling.len() < 2 {
                    continue;
                }
                if context == CorrectionContext::Prose && len == 1 {
                    continue;
                }
                // A word already on screen is what the user meant, and an everyday word is speech.
                if len == 1
                    && (vocabulary
                        .contains(words[index].trim_end_matches([',', '.', '?', '!', ';']))
                        || is_common_word(&heard))
                {
                    continue;
                }
                let all_common = span.iter().all(|word| is_common_word(word));
                let key = phonetic_key(&spelling);
                let needed = if len == 1 {
                    if key.len() < SINGLE_WORD_MIN_KEY {
                        continue;
                    }
                    threshold.max(SINGLE_WORD_THRESHOLD)
                } else {
                    threshold
                };
                let candidate = vocabulary
                    .terms
                    .iter()
                    .filter(|term| {
                        term.text != words[index..index + len].join(" ")
                            && is_vocabulary_term(&term.text)
                            && (context == CorrectionContext::Command
                                || is_identifier_shaped(&term.text))
                            && heard != term.text.to_lowercase()
                            // Everyday words only become a same-length plain term ("get
                            // status" as `git status`), never an identifier or flag.
                            && (!all_common
                                || !is_identifier_shaped(&term.text)
                                    && term.text.split_whitespace().count() == len)
                            && !log.is_suppressed(&heard, &term.text)
                            && term.spellings.iter().any(|term_spelling| {
                                similarity(&spelling, term_spelling) >= MIN_SPELLING_SIMILARITY
                            })
                    })
                    .map(|term| (term, confidence(&spelling, &key, term)))
                    .filter(|(_, score)| *score >= needed)
                    .max_by(|a, b| a.1.total_cmp(&b.1));
                // A leading word that doesn't belong to the term ("cd" in "cd atm design
                // analyzer") stays; the term starts at a later word.
                let leading_word_belongs = |term: &Term, score: f32| {
                    len == 1 || {
                        let rest: String = keys[index + 1..index + len]
                            .concat()
                            .chars()
                            .filter(|c| c.is_alphanumeric())
                            .collect();
                        confidence(&rest, &phonetic_key(&rest), term) < score
                    }
                };
                if let Some((term, score)) = candidate
                    && leading_word_belongs(term, score)
                    && best
                        .as_ref()
                        .is_none_or(|(_, _, best_score, _)| score > *best_score)
                {
                    best = Some((len, term.text.clone(), score, false));
                }
            }
        }
        if !out.is_empty() {
            out.push(' ');
        }
        match best {
            Some((len, target, score, pinned)) => {
                let start = out.len();
                out.push_str(&target);
                let end = out.len();
                out.push_str(trailing_punctuation(words[index + len - 1]));
                corrections.push(Correction {
                    heard: words[index..index + len]
                        .join(" ")
                        .trim_end_matches([',', '.', '?', '!', ';'])
                        .to_owned(),
                    corrected: target,
                    range: start..end,
                    confidence: score,
                    pinned,
                });
                index += len;
            }
            None => {
                out.push_str(words[index]);
                index += 1;
            }
        }
    }
    Corrected {
        text: out,
        corrections,
    }
}

/// What the user did with one heard-to-corrected mapping.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub heard: String,
    pub corrected: String,
    /// Times the correction was made.
    pub count: u32,
    /// Times the user kept it.
    pub accepted: u32,
    /// Times the user reverted it.
    pub reverted: u32,
    /// Unix seconds of the last correction or feedback.
    pub last_seen: u64,
    /// Always map `heard` to `corrected`, before any vocabulary match.
    #[serde(default)]
    pub pinned: bool,
    /// Never apply this mapping.
    #[serde(default)]
    pub disabled: bool,
}

impl LogEntry {
    /// Whether the mapping should not be applied: disabled, or reverted more than kept.
    pub fn is_suppressed(&self) -> bool {
        !self.pinned
            && (self.disabled
                || self.reverted >= MIN_REVERTS_TO_SUPPRESS && self.reverted >= self.accepted)
    }
}

/// The corrections the user has seen, keyed by heard words and corrected term. Serialized as a
/// JSON array of [`LogEntry`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CorrectionLog {
    entries: Vec<LogEntry>,
    index: HashMap<(String, String), usize>,
}

impl CorrectionLog {
    pub fn from_entries(entries: Vec<LogEntry>) -> Self {
        let mut log = Self::default();
        for mut entry in entries {
            entry.heard = heard_key(&entry.heard);
            if entry.heard.is_empty() || entry.corrected.is_empty() {
                continue;
            }
            log.entry_mut(&entry.heard.clone(), &entry.corrected.clone(), 0);
            let slot = log.index[&(entry.heard.clone(), entry.corrected.clone())];
            log.entries[slot] = entry;
        }
        log
    }

    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str::<Vec<LogEntry>>(json).map(Self::from_entries)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self.entries).unwrap_or_else(|_| "[]".to_owned())
    }

    pub fn entries(&self) -> &[LogEntry] {
        &self.entries
    }

    fn entry_mut(&mut self, heard: &str, corrected: &str, now: u64) -> &mut LogEntry {
        let heard = heard_key(heard);
        let key = (heard.clone(), corrected.to_owned());
        let slot = *self.index.entry(key).or_insert_with(|| {
            self.entries.push(LogEntry {
                heard,
                corrected: corrected.to_owned(),
                last_seen: now,
                ..LogEntry::default()
            });
            self.entries.len() - 1
        });
        let entry = &mut self.entries[slot];
        entry.last_seen = entry.last_seen.max(now);
        entry
    }

    fn get(&self, heard: &str, corrected: &str) -> Option<&LogEntry> {
        self.index
            .get(&(heard.to_owned(), corrected.to_owned()))
            .map(|slot| &self.entries[*slot])
    }

    /// The pinned target for `heard` (a [`heard_key`]), if any.
    pub fn pinned_target(&self, heard: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.pinned && !entry.disabled && entry.heard == heard)
            .map(|entry| entry.corrected.as_str())
    }

    /// Whether mapping `heard` (a [`heard_key`]) to `corrected` is suppressed.
    pub fn is_suppressed(&self, heard: &str, corrected: &str) -> bool {
        self.get(heard, corrected)
            .is_some_and(LogEntry::is_suppressed)
    }

    pub fn record_correction(&mut self, heard: &str, corrected: &str, now: u64) {
        self.entry_mut(heard, corrected, now).count += 1;
    }

    pub fn record_accepted(&mut self, heard: &str, corrected: &str, now: u64) {
        self.entry_mut(heard, corrected, now).accepted += 1;
    }

    pub fn record_reverted(&mut self, heard: &str, corrected: &str, now: u64) {
        self.entry_mut(heard, corrected, now).reverted += 1;
    }

    /// Pins `heard` to `corrected`, unpinning any other target for the same words.
    pub fn pin(&mut self, heard: &str, corrected: &str, now: u64) {
        let key = heard_key(heard);
        for entry in &mut self.entries {
            if entry.heard == key {
                entry.pinned = false;
            }
        }
        let entry = self.entry_mut(heard, corrected, now);
        entry.pinned = true;
        entry.disabled = false;
    }

    pub fn unpin(&mut self, heard: &str, corrected: &str) {
        if let Some(slot) = self.index.get(&(heard_key(heard), corrected.to_owned())) {
            self.entries[*slot].pinned = false;
        }
    }

    pub fn set_disabled(&mut self, heard: &str, corrected: &str, disabled: bool, now: u64) {
        let entry = self.entry_mut(heard, corrected, now);
        entry.disabled = disabled;
        if disabled {
            entry.pinned = false;
        }
    }

    pub fn remove(&mut self, heard: &str, corrected: &str) {
        let key = (heard_key(heard), corrected.to_owned());
        if self.index.remove(&key).is_some() {
            let entries = std::mem::take(&mut self.entries);
            *self = Self::from_entries(
                entries
                    .into_iter()
                    .filter(|e| (e.heard.as_str(), e.corrected.as_str()) != (&key.0, &key.1))
                    .collect(),
            );
        }
    }
}

#[cfg(test)]
#[path = "context_correction_tests.rs"]
mod tests;
