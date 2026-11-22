//! Pattern-based redaction of secrets in a recorded text stream.
//!
//! Text arrives in arbitrary PTY-sized chunks, so a secret can be split across chunks. The
//! redactor holds back the trailing unfinished word (and any unterminated private-key block)
//! until more text arrives, and matches against a tail of already-written text so that a secret
//! whose prefix (`Bearer `, `token=`) was written earlier is still caught.

use regex::Regex;

pub const REDACTED: &str = "[REDACTED]";

/// Already-written text kept as match context, in bytes.
const CONTEXT_LEN: usize = 256;
/// A trailing word longer than this is written even if unfinished.
const MAX_HELD_WORD: usize = 4096;
/// An unterminated private-key block longer than this is dropped as redacted.
const MAX_HELD_KEY_BLOCK: usize = 64 * 1024;
const KEY_BLOCK_START: &str = "-----BEGIN ";

/// `(pattern, replacement)`; the replacement may reference capture groups.
const BUILT_IN_PATTERNS: &[(&str, &str)] = &[
    (
        r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        REDACTED,
    ),
    (r"\b(?:sk|csk)-[A-Za-z0-9_-]{16,}", REDACTED),
    (r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", REDACTED),
    (r"\bgh[pousr]_[A-Za-z0-9]{30,}", REDACTED),
    (r"\bgithub_pat_[A-Za-z0-9_]{20,}", REDACTED),
    (r"\bxox[abposr]-[A-Za-z0-9-]{10,}", REDACTED),
    (
        r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
        REDACTED,
    ),
    (
        r"(?i)(authorization\s*[:=]\s*(?:bearer|basic|token)?\s*)[A-Za-z0-9._~+/=-]{8,}",
        "${1}[REDACTED]",
    ),
    (
        r"(?i)(\bbearer\s+)[A-Za-z0-9._~+/=-]{12,}",
        "${1}[REDACTED]",
    ),
    (
        r"([A-Za-z][A-Za-z0-9+.-]*://[^\s:/@\[\]]+:)[^\s@/\[\]]+@",
        "${1}[REDACTED]@",
    ),
    (
        r#"(?i)(\b[A-Za-z0-9_]*(?:password|passwd|secret|token|api[_-]?key|access[_-]?key)\s*[=:]\s*)(?:"[^"\s]+"|'[^'\s]+'|[^\s"'\[]+)"#,
        "${1}[REDACTED]",
    ),
];

struct Pattern {
    regex: Regex,
    replacement: String,
}

struct Match {
    start: usize,
    end: usize,
    replacement: String,
}

pub struct Redactor {
    patterns: Vec<Pattern>,
    /// Tail of text already returned, for context only.
    context: String,
    /// Text not yet returned.
    pending: String,
}

impl Redactor {
    /// The built-in patterns plus `extra` ones (whole matches replaced). Invalid extra patterns
    /// are logged and skipped.
    pub fn new<'a>(extra: impl IntoIterator<Item = &'a str>) -> Self {
        let mut patterns: Vec<_> = BUILT_IN_PATTERNS
            .iter()
            .map(|(pattern, replacement)| Pattern {
                regex: Regex::new(pattern).expect("built-in redaction pattern is valid"),
                replacement: (*replacement).to_owned(),
            })
            .collect();
        for pattern in extra {
            let pattern = pattern.trim();
            if pattern.is_empty() {
                continue;
            }
            match Regex::new(pattern) {
                Ok(regex) => patterns.push(Pattern {
                    regex,
                    replacement: REDACTED.to_owned(),
                }),
                Err(error) => log::warn!("Ignoring invalid redaction pattern: {error}"),
            }
        }
        Self {
            patterns,
            context: String::new(),
            pending: String::new(),
        }
    }

    /// Adds text and returns the redacted text that is safe to write now.
    pub fn push(&mut self, text: &str) -> String {
        self.pending.push_str(text);
        self.take(false)
    }

    /// Returns everything still held, redacted.
    pub fn flush(&mut self) -> String {
        self.take(true)
    }

    fn take(&mut self, flush: bool) -> String {
        let ctx_len = self.context.len();
        let combined = format!("{}{}", self.context, self.pending);
        let matches = self.find_matches(&combined, ctx_len);

        let mut cut = if flush {
            self.pending.len()
        } else {
            self.hold_point()
        };
        // Never split a match: hold it back whole.
        for m in &matches {
            let start = m.start.saturating_sub(ctx_len);
            let end = m.end - ctx_len;
            if start < cut && cut < end {
                cut = start;
            }
        }

        let mut out = String::new();
        let mut written = 0;
        for m in matches {
            let start = m.start.saturating_sub(ctx_len);
            let end = m.end - ctx_len;
            if end > cut || start < written {
                continue;
            }
            out.push_str(&self.pending[written..start]);
            if m.start < ctx_len {
                // Part of the match was already written; keep the replacement's matching prefix
                // out of the output.
                let written_part = &combined[m.start..ctx_len];
                out.push_str(m.replacement.strip_prefix(written_part).unwrap_or(REDACTED));
            } else {
                out.push_str(&m.replacement);
            }
            written = end;
        }
        out.push_str(&self.pending[written..cut]);

        let held = self.pending.split_off(cut);
        self.pending = held;
        self.context.push_str(&out);
        if self.context.len() > CONTEXT_LEN {
            let mut start = self.context.len() - CONTEXT_LEN;
            while !self.context.is_char_boundary(start) {
                start += 1;
            }
            self.context.drain(..start);
        }
        out
    }

    /// Matches that end inside `pending`, sorted and non-overlapping, earliest pattern first.
    fn find_matches(&self, combined: &str, ctx_len: usize) -> Vec<Match> {
        let mut matches = Vec::new();
        for pattern in &self.patterns {
            for caps in pattern.regex.captures_iter(combined) {
                let whole = caps.get(0).expect("group 0 always matches");
                if whole.end() <= ctx_len || whole.is_empty() {
                    continue;
                }
                let mut replacement = String::new();
                caps.expand(&pattern.replacement, &mut replacement);
                matches.push(Match {
                    start: whole.start(),
                    end: whole.end(),
                    replacement,
                });
            }
        }
        matches.sort_by_key(|m| (m.start, std::cmp::Reverse(m.end)));
        let mut merged: Vec<Match> = Vec::new();
        for m in matches {
            if merged.last().is_some_and(|last| m.start < last.end) {
                continue;
            }
            merged.push(m);
        }
        merged
    }

    /// Where the safe-to-write prefix of `pending` ends.
    fn hold_point(&mut self) -> usize {
        let pending = &self.pending;
        let word_start = pending
            .char_indices()
            .rev()
            .find(|(_, ch)| ch.is_whitespace())
            .map_or(0, |(index, ch)| index + ch.len_utf8());
        let mut cut = if pending.len() - word_start > MAX_HELD_WORD {
            pending.len()
        } else {
            word_start
        };
        if let Some(block_start) = pending.rfind(KEY_BLOCK_START)
            && is_open_key_block(&pending[block_start..])
        {
            if pending.len() - block_start > MAX_HELD_KEY_BLOCK {
                self.pending.truncate(block_start);
                self.pending.push_str(REDACTED);
                return self.pending.len();
            }
            cut = cut.min(block_start);
        }
        cut
    }
}

/// Whether `tail`, starting at `-----BEGIN `, is (or may still become) a private-key block that
/// hasn't ended yet.
fn is_open_key_block(tail: &str) -> bool {
    let is_key = match tail.split_once('\n') {
        Some((header, _)) => header.contains("PRIVATE KEY"),
        None => true,
    };
    let has_ended = tail
        .find("-----END ")
        .is_some_and(|end| tail[end..].contains("PRIVATE KEY-----"));
    is_key && !has_ended
}

#[cfg(test)]
#[path = "redact_tests.rs"]
mod tests;
