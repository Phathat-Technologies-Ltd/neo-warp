//! Splits a streamed markdown reply into sentences that can be spoken as soon as each one is
//! complete, without reading markdown syntax aloud.

use super::tts_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkerOptions {
    /// Whether a code block is replaced by a short summary; when off it is skipped silently.
    pub read_code_summaries: bool,
}

impl Default for ChunkerOptions {
    fn default() -> Self {
        Self {
            read_code_summaries: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenFence {
    marker: String,
    language: Option<String>,
    lines: usize,
}

/// Turns text deltas into spoken sentences, normalized with [`tts_text::normalize`].
#[derive(Debug, Clone, Default)]
pub struct SentenceChunker {
    options: ChunkerOptions,
    /// Text received but not yet consumed.
    pending: String,
    /// Whether `pending` starts in the middle of a line, after an emitted sentence.
    mid_line: bool,
    /// Prose of the sentence being built, markdown already stripped.
    sentence: String,
    fence: Option<OpenFence>,
}

impl SentenceChunker {
    pub fn new(options: ChunkerOptions) -> Self {
        Self {
            options,
            ..Self::default()
        }
    }

    /// Adds a delta of the reply and returns the sentences it completed.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.pending.push_str(delta);
        let mut out = Vec::new();
        while let Some(newline) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=newline).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            self.consume_line(line, &mut out);
            self.mid_line = false;
        }
        self.consume_partial_line(&mut out);
        out
    }

    /// Ends the reply and returns what is left, including an unclosed code block's summary.
    pub fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let rest = std::mem::take(&mut self.pending);
        if !rest.is_empty() {
            self.consume_line(&rest, &mut out);
        }
        if let Some(fence) = self.fence.take() {
            self.emit_code_summary(fence, &mut out);
        }
        self.flush_sentence(&mut out);
        self.mid_line = false;
        out
    }

    fn consume_line(&mut self, line: &str, out: &mut Vec<String>) {
        if let Some(fence) = &mut self.fence {
            if line.trim_start().starts_with(fence.marker.as_str()) && !self.mid_line {
                let fence = self.fence.take().expect("checked above");
                self.emit_code_summary(fence, out);
            } else {
                fence.lines += 1;
            }
            return;
        }
        if !self.mid_line {
            let trimmed = line.trim_start();
            if let Some(marker) = fence_marker(trimmed) {
                self.flush_sentence(out);
                let language = trimmed[marker.len()..].trim();
                self.fence = Some(OpenFence {
                    marker: marker.to_owned(),
                    language: (!language.is_empty()).then(|| language.to_owned()),
                    lines: 0,
                });
                return;
            }
            if trimmed.is_empty() {
                self.flush_sentence(out);
                return;
            }
            if is_unspoken_line(trimmed) {
                return;
            }
            let (prose, ends_sentence) = strip_line_prefix(trimmed);
            if ends_sentence {
                self.flush_sentence(out);
            }
            self.add_prose(prose, out);
            if ends_sentence {
                self.flush_sentence(out);
            }
            return;
        }
        self.add_prose(line, out);
    }

    /// Emits the sentences already complete in an unfinished line, leaving the rest pending.
    fn consume_partial_line(&mut self, out: &mut Vec<String>) {
        if self.fence.is_some() || self.pending.is_empty() {
            return;
        }
        if !self.mid_line {
            let trimmed = self.pending.trim_start();
            // The line could still turn out to be a fence, a table row, or a list item.
            if trimmed.len() < 4 || is_unspoken_line(trimmed) || fence_marker(trimmed).is_some() {
                return;
            }
        }
        let Some(end) = last_sentence_end(&self.pending) else {
            return;
        };
        let head: String = self.pending.drain(..end).collect();
        if self.mid_line {
            self.add_prose(&head, out);
        } else {
            self.consume_line(&head, out);
        }
        self.flush_sentence(out);
        self.mid_line = true;
    }

    fn add_prose(&mut self, text: &str, out: &mut Vec<String>) {
        let text = strip_inline_markdown(text);
        let mut rest = text.as_str();
        while let Some(end) = first_sentence_end(rest) {
            self.append(&rest[..end]);
            self.flush_sentence(out);
            rest = &rest[end..];
        }
        self.append(rest);
    }

    fn append(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if !self.sentence.is_empty() {
            self.sentence.push(' ');
        }
        self.sentence.push_str(text);
    }

    fn flush_sentence(&mut self, out: &mut Vec<String>) {
        let sentence = tts_text::normalize(&std::mem::take(&mut self.sentence));
        if sentence.chars().any(char::is_alphanumeric) {
            out.push(sentence);
        }
    }

    fn emit_code_summary(&mut self, fence: OpenFence, out: &mut Vec<String>) {
        self.flush_sentence(out);
        if self.options.read_code_summaries {
            out.push(code_summary(fence.language.as_deref(), fence.lines));
        }
    }
}

/// What is said in place of a code block.
pub fn code_summary(language: Option<&str>, lines: usize) -> String {
    let lines = match lines {
        1 => "1 line".to_owned(),
        lines => format!("{lines} lines"),
    };
    match language {
        Some(language) => format!("I wrote a code block in {language}, {lines}."),
        None => format!("I wrote a code block, {lines}."),
    }
}

fn fence_marker(line: &str) -> Option<&'static str> {
    ["```", "~~~"]
        .into_iter()
        .find(|marker| line.starts_with(marker))
}

/// Table rows, rules, and images are not read aloud.
fn is_unspoken_line(line: &str) -> bool {
    line.starts_with('|')
        || line.starts_with("![")
        || (line.len() >= 3 && line.chars().all(|c| matches!(c, '-' | '*' | '_' | ' ')))
}

/// Strips a heading, quote, or list marker, saying whether the line stands as its own sentence.
fn strip_line_prefix(line: &str) -> (&str, bool) {
    let heading = line.trim_start_matches('#');
    if heading.len() != line.len() && heading.starts_with(' ') {
        return (heading.trim_start(), true);
    }
    if let Some(rest) = line.strip_prefix("> ") {
        return (rest, false);
    }
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return (rest, true);
        }
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0
        && let Some(rest) = line[digits..]
            .strip_prefix(". ")
            .or_else(|| line[digits..].strip_prefix(") "))
    {
        return (rest, true);
    }
    (line, false)
}

/// Removes emphasis markers, link targets, and inline code backticks.
fn strip_inline_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        match c {
            '*' | '`' | '~' => {}
            '_' => {
                let inside_word = index > 0
                    && chars[index - 1].is_alphanumeric()
                    && chars.get(index + 1).is_some_and(|c| c.is_alphanumeric());
                if inside_word {
                    out.push(c);
                }
            }
            ']' if chars.get(index + 1) == Some(&'(') => {
                // Skip the link target.
                match chars[index..].iter().position(|&c| c == ')') {
                    Some(close) => index += close,
                    None => index = chars.len(),
                }
            }
            '[' => {}
            c => out.push(c),
        }
        index += 1;
    }
    out
}

/// The byte offset just past the first sentence end (a terminator followed by whitespace).
fn first_sentence_end(text: &str) -> Option<usize> {
    sentence_ends(text).next()
}

fn last_sentence_end(text: &str) -> Option<usize> {
    sentence_ends(text).last()
}

fn sentence_ends(text: &str) -> impl Iterator<Item = usize> + '_ {
    let bytes = text.as_bytes();
    (0..bytes.len().saturating_sub(1)).filter_map(move |index| {
        let is_end = matches!(bytes[index], b'.' | b'!' | b'?')
            && bytes[index + 1].is_ascii_whitespace()
            // "e.g. " and "1. " are not sentence ends.
            && index > 0
            && !bytes[index - 1].is_ascii_digit()
            && !(index >= 2 && bytes[index - 2] == b'.');
        is_end.then_some(index + 1)
    })
}

#[cfg(test)]
#[path = "chunker_tests.rs"]
mod tests;
