//! Removes speech disfluencies from transcripts: fillers ("uh", "um"), repeated words ("what
//! what"), cut-off words ("gi give"), and restarts ("go to the, uh, open the file"). Only whole
//! words are removed; the rest of the transcript is kept as spoken, so grammar is never rewritten.
//!
//! A rule pass handles the unambiguous cases and is cheap enough for partial transcripts. A
//! [`DisfluencyScorer`] (a small token classifier) adds restarts and fillers whose role depends on
//! context, such as "like" or "you know".

use std::collections::HashMap;

use crate::TranscribeError;

/// The delete probability above which the model removes a word. High because a kept filler costs
/// less than a dropped word.
pub const DEFAULT_THRESHOLD: f32 = 0.85;

/// When the model would remove more than this share of an utterance's words, its output is
/// distrusted and only the rule pass applies.
const MAX_MODEL_DELETE_SHARE: f32 = 0.6;

const FILLERS: &[&str] = &[
    "uh", "uhh", "um", "umm", "uhm", "er", "erm", "eh", "hmm", "hm", "mm", "mmm", "ah",
];

/// Spoken terminal symbols that [`crate::terminal_text`] turns into syntax; "dot dot" is `..`.
const SPOKEN_SYMBOLS: &[&str] = &[
    "dash",
    "minus",
    "dot",
    "slash",
    "backslash",
    "tilde",
    "underscore",
    "pipe",
    "dollar",
    "colon",
    "equals",
    "star",
    "asterisk",
];

/// Command-line tools whose names are rarely English words. A tool name and the word after it
/// (its subcommand, as in "pip install") are kept, so "npm run build" isn't read as a restart.
const CLI_TOOLS: &[&str] = &[
    "git",
    "gh",
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "pip",
    "pip3",
    "uv",
    "cargo",
    "rustup",
    "ls",
    "rm",
    "cd",
    "mv",
    "cp",
    "mkdir",
    "rmdir",
    "chmod",
    "chown",
    "sudo",
    "docker",
    "kubectl",
    "brew",
    "python",
    "python3",
    "node",
    "ssh",
    "scp",
    "rsync",
    "curl",
    "wget",
    "grep",
    "rg",
    "sed",
    "awk",
    "jq",
    "tar",
    "vim",
    "nvim",
    "tmux",
    "terraform",
    "pytest",
    "apt",
    "systemctl",
    "ps",
    "du",
    "df",
];

/// Words whose immediate repetition is almost always a stutter. Content words aren't listed,
/// since "hello hello" or "bye bye" may be meant; the model judges those.
const REPEATABLE: &[&str] = &[
    "a", "an", "the", "i", "i'm", "i'd", "i'll", "you", "we", "they", "he", "she", "it", "it's",
    "what", "what's", "why", "how", "where", "when", "which", "who", "is", "are", "was", "can",
    "could", "would", "will", "should", "do", "does", "did", "to", "of", "in", "on", "for", "with",
    "and", "but", "or", "if", "my", "your", "this", "these", "those", "there", "let's", "so", "me",
    "at", "from", "about", "into", "show", "please", "just",
];

/// Words that are fillers or discourse markers when the model marks them.
const DISCOURSE_WORDS: &[&str] = &[
    "so",
    "well",
    "okay",
    "ok",
    "like",
    "yeah",
    "basically",
    "you",
    "know",
    "i",
    "mean",
    "uh",
    "um",
    "oh",
    "ah",
    "hmm",
    "er",
    "eh",
    "sorry",
    "no",
    "wait",
    "actually",
];

/// Word doubles that are grammatical ("I know that that works").
const LEGIT_DOUBLES: &[&str] = &["that", "had"];

/// Words that carry grammar rather than content; the model may not remove one alone.
const FUNCTION_WORDS: &[&str] = &[
    "a", "an", "the", "of", "to", "in", "on", "at", "for", "with", "from", "by", "about", "into",
    "and", "or", "but", "if", "that", "this", "these", "those", "it", "is", "are", "was", "be",
    "do", "does", "did", "not", "my", "your", "our", "their", "its", "as", "than", "then", "what",
    "which", "who", "how", "when", "where", "why", "can", "could", "would", "should", "will", "me",
    "you", "we", "they", "he", "she", "i", "us", "them", "there", "here",
];

/// Common short words that a cut-off fragment must not be mistaken for ("I in", "up update").
const SHORT_WORDS: &[&str] = &[
    "a", "an", "as", "at", "be", "by", "do", "go", "he", "hi", "i", "if", "in", "is", "it", "me",
    "my", "no", "of", "oh", "ok", "on", "or", "so", "to", "up", "us", "we", "all", "and", "any",
    "are", "but", "can", "did", "for", "get", "had", "has", "her", "him", "his", "how", "its",
    "let", "new", "not", "now", "off", "old", "one", "our", "out", "own", "put", "run", "say",
    "see", "set", "she", "the", "too", "two", "use", "was", "way", "who", "why", "yes", "yet",
    "you", "add", "big", "end", "few", "fix", "log", "man", "max", "min", "top", "try", "also",
    "back", "been", "both", "come", "each", "even", "find", "from", "give", "have", "here", "into",
    "just", "like", "look", "make", "more", "most", "much", "must", "only", "open", "over", "some",
    "take", "than", "that", "them", "then", "they", "this", "very", "want", "well", "what", "when",
    "will", "with", "work",
];

/// Longest repeated phrase the rule pass removes.
const MAX_REPEAT_WORDS: usize = 5;

/// Longest cut-off fragment the rule pass removes; longer ones are left to the model.
const MAX_FRAGMENT_CHARS: usize = 4;

/// Scores how likely each word of an utterance is a disfluency.
pub trait DisfluencyScorer: Send + Sync {
    /// The probability that each of `words` should be removed, one per word.
    fn delete_probabilities(&self, words: &[&str]) -> Result<Vec<f32>, TranscribeError>;
}

/// Removes disfluencies with the rule pass only. Words matching `protected` (case-insensitively)
/// and terminal syntax are never removed.
pub fn remove_disfluencies(text: &str, protected: &[String]) -> String {
    clean(text, protected, None, DEFAULT_THRESHOLD)
}

/// Removes disfluencies with the rule pass and, when given, `scorer`. Words matching `protected`
/// (case-insensitively), terminal syntax, and quoted text are never removed. If the scorer fails,
/// only the rule pass applies.
pub fn clean(
    text: &str,
    protected: &[String],
    scorer: Option<&dyn DisfluencyScorer>,
    threshold: f32,
) -> String {
    clean_passes(text, protected, true, scorer, threshold)
}

/// [`clean`] without the rule pass, for evaluating the model alone.
#[doc(hidden)]
pub fn clean_model_only(
    text: &str,
    protected: &[String],
    scorer: &dyn DisfluencyScorer,
    threshold: f32,
) -> String {
    clean_passes(text, protected, false, Some(scorer), threshold)
}

fn clean_passes(
    text: &str,
    protected: &[String],
    rules: bool,
    scorer: Option<&dyn DisfluencyScorer>,
    threshold: f32,
) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return String::new();
    }
    let keys: Vec<String> = words.iter().map(|w| key(w)).collect();
    let locked = locked_words(&words, &keys, protected);
    let mut delete = if rules {
        rule_deletions(&words, &keys, &locked)
    } else {
        vec![false; words.len()]
    };
    if let Some(scorer) = scorer {
        match scorer.delete_probabilities(&words) {
            Ok(probabilities) if probabilities.len() == words.len() => {
                let model_locked = model_locked_words(&keys, &locked);
                let mut model: Vec<bool> = probabilities
                    .iter()
                    .zip(&model_locked)
                    .map(|(p, locked)| *p > threshold && !locked)
                    .collect();
                keep_lone_function_words(&keys, &delete, &mut model);
                require_restart_evidence(&words, &keys, &delete, &mut model);
                let removed = model
                    .iter()
                    .zip(&delete)
                    .filter(|(m, r)| **m || **r)
                    .count();
                if (removed as f32) <= MAX_MODEL_DELETE_SHARE * words.len() as f32 {
                    for (d, m) in delete.iter_mut().zip(model) {
                        *d |= m;
                    }
                }
            }
            Ok(probabilities) => log::warn!(
                "Disfluency model returned {} scores for {} words",
                probabilities.len(),
                words.len()
            ),
            Err(error) => log::warn!("Disfluency model failed: {error}"),
        }
    }
    reconstruct(&words, &delete)
}

/// Undoes model removals of a lone function word mid-utterance ("fix of the typo"), which drop
/// meaning rather than a disfluency, unless the word repeats or starts the next kept word.
fn keep_lone_function_words(keys: &[String], rules: &[bool], model: &mut [bool]) {
    let removed = |i: usize, model: &[bool]| model[i] || rules[i];
    for i in 1..keys.len() {
        if !model[i] || rules[i] || removed(i - 1, model) {
            continue;
        }
        if i + 1 < keys.len() && removed(i + 1, model) {
            continue;
        }
        if !FUNCTION_WORDS.contains(&keys[i].as_str()) {
            continue;
        }
        let next = (i + 1..keys.len()).find(|j| !removed(*j, model));
        let echoed = next.is_some_and(|j| keys[j].starts_with(keys[i].as_str()));
        if !echoed {
            model[i] = false;
        }
    }
}

/// Undoes model removals that have no sign of being disfluent. A removed span must be discourse
/// words set off by a comma ("so,", "you know,"), a cut-off fragment, or the start of a restart that the following
/// words echo ("go to the, uh, open the file"); a span the speaker never returns to ("I mean what
/// I said, delete it") is meant. Grammatical doubles ("that that", "had had") are kept.
fn require_restart_evidence(words: &[&str], keys: &[String], rules: &[bool], model: &mut [bool]) {
    let n = keys.len();
    let mut start = 0;
    while start < n {
        if !(model[start] && !rules[start]) {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < n && (model[end] || rules[end]) {
            end += 1;
        }
        let span: Vec<usize> = (start..end).filter(|i| model[*i] && !rules[*i]).collect();
        let following: Vec<&str> = (end..n)
            .filter(|i| !model[*i] && !rules[*i])
            .take(span.len() + 4)
            .map(|i| keys[i].as_str())
            .collect();
        // Discourse words set off by a comma: "I mean, the server" but not "I mean the server".
        let discourse = words[end - 1].ends_with(',')
            && span
                .iter()
                .all(|i| DISCOURSE_WORDS.contains(&keys[*i].as_str()));
        let echoed = span.iter().any(|i| {
            let word = keys[*i].as_str();
            !DISCOURSE_WORDS.contains(&word)
                && following.iter().any(|next| {
                    *next == word || (next.starts_with(word) && word.len() < next.len())
                })
        });
        let double = span.len() == 1
            && LEGIT_DOUBLES.contains(&keys[span[0]].as_str())
            && following.first() == Some(&keys[span[0]].as_str());
        if !(discourse || echoed) || double {
            for i in span {
                model[i] = false;
            }
        }
        start = end;
    }
}

/// Lowercases `word` and strips surrounding punctuation, keeping inner apostrophes and hyphens.
fn key(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .trim_matches('\'')
        .to_lowercase()
}

/// Whether `word` looks like terminal syntax: a path, flag, file name, variable, or number.
fn is_shell_like(word: &str) -> bool {
    let trimmed = word.trim_end_matches([',', '.', '?', '!', ';', ':']);
    // Punctuation alone, such as `..` or `--`.
    if trimmed.is_empty() {
        return !word.is_empty();
    }
    if trimmed.starts_with('-') && trimmed.len() > 1 {
        return true;
    }
    if trimmed.chars().any(|c| {
        matches!(
            c,
            '/' | '\\'
                | '_'
                | '='
                | '$'
                | '~'
                | '@'
                | '*'
                | '|'
                | '<'
                | '>'
                | '&'
                | ';'
                | '{'
                | '}'
                | '['
                | ']'
                | '('
                | ')'
                | '#'
                | '%'
                | '^'
                | '+'
                | '`'
        )
    }) {
        return true;
    }
    // A dot between characters: `main.rs`, `1.2`, `..`.
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.windows(2).any(|w| w[0] == '.' && w[1] != '.') {
        return true;
    }
    trimmed.chars().any(|c| c.is_ascii_digit())
        || (trimmed.chars().skip(1).any(char::is_uppercase)
            && trimmed.chars().any(char::is_lowercase))
}

/// [`locked_words`] plus command-line tool names and the word after each, which only the model
/// may not remove.
fn model_locked_words(keys: &[String], locked: &[bool]) -> Vec<bool> {
    let mut model_locked = locked.to_vec();
    for i in 0..keys.len() {
        if CLI_TOOLS.contains(&keys[i].as_str()) {
            let end = (i + 2).min(keys.len());
            model_locked[i..end].fill(true);
        }
    }
    model_locked
}

/// Words never removed: protected terms, terminal syntax, spoken symbols, and quoted text.
fn locked_words(words: &[&str], keys: &[String], protected: &[String]) -> Vec<bool> {
    let protected_keys: Vec<Vec<String>> = protected
        .iter()
        .map(|term| term.split_whitespace().map(key).collect::<Vec<_>>())
        .filter(|term: &Vec<String>| !term.is_empty() && term.iter().all(|k| !k.is_empty()))
        .collect();
    let mut locked: Vec<bool> = words
        .iter()
        .zip(keys)
        .map(|(word, key)| is_shell_like(word) || SPOKEN_SYMBOLS.contains(&key.as_str()))
        .collect();
    for term in &protected_keys {
        // Single common words (e.g. a bias term "the") would lock everything; require a real term.
        if term.len() == 1 && term[0].len() < 3 {
            continue;
        }
        for start in 0..words.len().saturating_sub(term.len() - 1) {
            if keys[start..start + term.len()] == term[..] {
                locked[start..start + term.len()].fill(true);
            }
        }
    }
    let mut in_quote = false;
    for (i, word) in words.iter().enumerate() {
        let quotes = word.matches(['"', '`']).count();
        if in_quote || quotes > 0 {
            locked[i] = true;
        }
        if quotes % 2 == 1 {
            in_quote = !in_quote;
        }
    }
    locked
}

fn rule_deletions(words: &[&str], keys: &[String], locked: &[bool]) -> Vec<bool> {
    let n = words.len();
    let mut delete = vec![false; n];
    let removable = |i: usize, delete: &[bool]| !locked[i] && !delete[i] && !keys[i].is_empty();

    for i in 0..n {
        let is_unit = keys[i] == "mm" && i > 0 && keys[i - 1].chars().all(|c| c.is_ascii_digit());
        if removable(i, &delete) && FILLERS.contains(&keys[i].as_str()) && !is_unit {
            delete[i] = true;
        }
    }

    // Comma-set discourse markers: ", like," / "You know," / "I mean," at a clause start.
    for i in 0..n {
        let clause_start = i == 0
            || words[i - 1].ends_with([',', '.', '?', '!'])
            || (delete[i - 1] && (i < 2 || words[i - 2].ends_with([',', '.', '?', '!'])));
        if !clause_start {
            continue;
        }
        let phrase_len = if keys[i] == "like" {
            1
        } else if i + 1 < n
            && matches!(
                (keys[i].as_str(), keys[i + 1].as_str()),
                ("you", "know") | ("i", "mean")
            )
        {
            2
        } else {
            0
        };
        if phrase_len == 0 {
            continue;
        }
        let last = i + phrase_len - 1;
        let followed = last + 1 < n;
        if words[last].ends_with(',') && followed && (i..=last).all(|j| removable(j, &delete)) {
            delete[i..=last].fill(true);
        }
    }

    // Exact repeats of one to five words, removing the earlier copies. Deleted words between the
    // copies ("the, uh, the") are skipped over.
    let live: Vec<usize> = (0..n).filter(|i| !delete[*i]).collect();
    for len in (1..=MAX_REPEAT_WORDS).rev() {
        let mut a = 0;
        while a + 2 * len <= live.len() {
            let first = &live[a..a + len];
            let second = &live[a + len..a + 2 * len];
            let same = first
                .iter()
                .zip(second)
                .all(|(x, y)| !keys[*x].is_empty() && keys[*x] == keys[*y]);
            let word = keys[first[0]].as_str();
            let allowed = len > 1 || REPEATABLE.contains(&word) || CLI_TOOLS.contains(&word);
            let free = first
                .iter()
                .chain(second)
                .all(|i| !locked[*i] && !delete[*i]);
            // A sentence break between copies separates two sentences, not a stutter.
            let joined = !words[first[len - 1]].ends_with(['.', '?', '!']);
            if same && allowed && free && joined {
                for &i in first {
                    delete[i] = true;
                }
                a += len;
            } else {
                a += 1;
            }
        }
    }

    // Cut-off fragments that start the next word: "gi give", "st- start".
    for i in (0..n).rev() {
        if !removable(i, &delete) {
            continue;
        }
        let Some(next) = (i + 1..n).find(|j| !delete[*j] && *j != i) else {
            continue;
        };
        let fragment = keys[i].trim_end_matches('-');
        let hyphenated = keys[i].ends_with('-') || words[i].trim_end_matches(',').ends_with('-');
        let target = &keys[next];
        let is_fragment = !fragment.is_empty()
            && fragment.chars().all(char::is_alphabetic)
            && fragment.chars().count() <= MAX_FRAGMENT_CHARS
            && target.len() > fragment.len()
            && target.starts_with(fragment)
            && (hyphenated || (fragment.len() >= 2 && !SHORT_WORDS.contains(&fragment)));
        if is_fragment {
            delete[i] = true;
        }
    }
    delete
}

/// Joins the kept words, moving sentence punctuation and capitals off removed words and dropping
/// the comma that set off a removed aside.
fn reconstruct(words: &[&str], delete: &[bool]) -> String {
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut capitalize_next = false;
    let mut i = 0;
    while i < words.len() {
        if !delete[i] {
            let mut word = words[i].to_owned();
            if capitalize_next {
                word = capitalize(&word);
                capitalize_next = false;
            }
            out.push(word);
            i += 1;
            continue;
        }
        let start = i;
        while i < words.len() && delete[i] {
            i += 1;
        }
        let first = words[start];
        let last = words[i - 1];
        let at_sentence_start = out.last().is_none_or(|w| w.ends_with(['.', '?', '!']));
        if at_sentence_start && first.chars().next().is_some_and(char::is_uppercase) {
            capitalize_next = true;
        }
        let trailing: String = last
            .chars()
            .rev()
            .take_while(|c| matches!(c, '.' | '?' | '!' | ','))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let Some(previous) = out.last_mut() else {
            continue;
        };
        if trailing.contains(['.', '?', '!']) {
            if !previous.ends_with(['.', '?', '!']) {
                let terminal = trailing.trim_start_matches(',');
                *previous = format!("{}{terminal}", previous.trim_end_matches(','));
                capitalize_next = i < words.len();
            }
        } else if previous.ends_with(',') && (trailing.ends_with(',') || i == words.len()) {
            previous.pop();
        }
    }
    out.join(" ")
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(c) if c.is_lowercase() => c.to_uppercase().chain(chars).collect(),
        _ => word.to_owned(),
    }
}

/// An uncased BERT WordPiece tokenizer.
pub struct WordPiece {
    ids: HashMap<String, i64>,
    unk: i64,
    cls: i64,
    sep: i64,
}

/// Longest word the tokenizer splits; longer ones become `[UNK]`, as in BERT.
const MAX_WORD_CHARS: usize = 100;

impl WordPiece {
    /// Parses a `vocab.txt` (one piece per line, id = line number).
    pub fn from_vocab_txt(text: &str) -> Result<Self, TranscribeError> {
        let ids: HashMap<String, i64> = text
            .lines()
            .enumerate()
            .map(|(i, piece)| (piece.to_owned(), i as i64))
            .collect();
        let id = |piece: &str| {
            ids.get(piece).copied().ok_or_else(|| {
                TranscribeError::Engine(format!("the vocabulary has no {piece} token"))
            })
        };
        Ok(Self {
            unk: id("[UNK]")?,
            cls: id("[CLS]")?,
            sep: id("[SEP]")?,
            ids,
        })
    }

    pub fn cls(&self) -> i64 {
        self.cls
    }

    pub fn sep(&self) -> i64 {
        self.sep
    }

    /// The piece ids of one whitespace-separated word: lowercased, split at punctuation, then
    /// greedy longest-match WordPiece.
    pub fn word_ids(&self, word: &str) -> Vec<i64> {
        let lowered = strip_accents(&word.to_lowercase());
        let mut ids = Vec::new();
        let mut current = String::new();
        for c in lowered.chars() {
            if is_punctuation(c) {
                if !current.is_empty() {
                    self.push_pieces(&current, &mut ids);
                    current.clear();
                }
                self.push_pieces(&c.to_string(), &mut ids);
            } else if !c.is_control() {
                current.push(c);
            }
        }
        if !current.is_empty() {
            self.push_pieces(&current, &mut ids);
        }
        ids
    }

    fn push_pieces(&self, word: &str, ids: &mut Vec<i64>) {
        let chars: Vec<char> = word.chars().collect();
        if chars.len() > MAX_WORD_CHARS {
            ids.push(self.unk);
            return;
        }
        let mut pieces = Vec::new();
        let mut start = 0;
        while start < chars.len() {
            let mut end = chars.len();
            let mut found = None;
            while start < end {
                let mut piece: String = chars[start..end].iter().collect();
                if start > 0 {
                    piece.insert_str(0, "##");
                }
                if let Some(id) = self.ids.get(&piece) {
                    found = Some(*id);
                    break;
                }
                end -= 1;
            }
            let Some(id) = found else {
                ids.push(self.unk);
                return;
            };
            pieces.push(id);
            start = end;
        }
        ids.extend(pieces);
    }
}

/// BERT's punctuation: ASCII symbols plus Unicode punctuation.
fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(c, '\u{2010}'..='\u{2027}' | '\u{3000}'..='\u{303F}' | '\u{00A1}' | '\u{00BF}')
}

/// Folds the common Latin accented letters, as BERT's uncased tokenizer does.
fn strip_accents(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            'ý' | 'ÿ' => 'y',
            other => other,
        })
        .collect()
}

#[cfg(test)]
#[path = "disfluency_tests.rs"]
mod tests;
