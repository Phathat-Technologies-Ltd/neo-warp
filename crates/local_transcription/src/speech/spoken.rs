//! The part of an agent reply that conversation mode reads aloud: the `<spoken>` summary the agent
//! is asked to open each reply with, or, when a reply has none, a short plain-English readout
//! filtered from its prose.

const OPEN: &str = "<spoken>";
const CLOSE: &str = "</spoken>";

/// The most sentences the fallback readout keeps.
const FALLBACK_MAX_SENTENCES: usize = 2;
/// The most words the fallback readout keeps.
const FALLBACK_MAX_WORDS: usize = 40;
/// Numbers with more digits than this are not read aloud.
const MAX_SPOKEN_DIGITS: usize = 3;

/// Splits a (possibly still streaming) reply into its `<spoken>` summary and the text to show on
/// screen. While the summary streams, it is returned as far as it got and nothing is shown; a
/// reply that is only a prefix of the opening tag counts as an empty summary so the tag never
/// flashes on screen.
pub fn split_spoken_block(text: &str) -> (Option<String>, &str) {
    let trimmed = text.trim_start();
    if let Some(after) = trimmed.strip_prefix(OPEN) {
        return match after.find(CLOSE) {
            Some(end) => (
                Some(after[..end].trim().to_owned()),
                after[end + CLOSE.len()..].trim_start(),
            ),
            None => (
                Some(strip_partial_suffix(after, CLOSE).trim().to_owned()),
                "",
            ),
        };
    }
    if !trimmed.is_empty() && OPEN.starts_with(trimmed) {
        return (Some(String::new()), "");
    }
    (None, text)
}

/// Drops a trailing prefix of `tag`, such as `</spo` while `</spoken>` streams in.
fn strip_partial_suffix<'a>(text: &'a str, tag: &str) -> &'a str {
    (1..tag.len())
        .rev()
        .find(|&len| text.ends_with(&tag[..len]))
        .map_or(text, |len| &text[..text.len() - len])
}

/// A short readout of a reply that has no `<spoken>` summary: code, inline code, paths, URLs,
/// hashes, flags, long numbers, tables, lists, and markdown are dropped, and the first sentences
/// are kept. While the reply streams, a trailing partial word is left out so it is never spoken
/// before it is known to be readable.
pub fn fallback_readout(markdown: &str, finished: bool) -> String {
    let mut prose = Vec::new();
    let mut in_code = false;
    for line in markdown.lines() {
        let line = line.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code || line.is_empty() || is_list_or_table_line(line) {
            continue;
        }
        let line = line.trim_start_matches(['#', '>']).trim_start();
        prose.push(strip_inline_code(line));
    }
    let mut text = prose.join(" ");
    let ends_mid_word = !markdown.ends_with(char::is_whitespace) && !in_code;
    if !finished && ends_mid_word {
        let cut = text.rfind(char::is_whitespace).unwrap_or(0);
        text.truncate(cut);
    }

    let mut words: Vec<String> = Vec::new();
    let mut sentences = 0;
    for token in text.split_whitespace() {
        if words.len() >= FALLBACK_MAX_WORDS || sentences >= FALLBACK_MAX_SENTENCES {
            break;
        }
        let ends_sentence = token.ends_with(['.', '!', '?']);
        let core = token.trim_matches(|c: char| !c.is_alphanumeric());
        if core.is_empty() || is_technical(token, core) {
            if ends_sentence
                && !core.is_empty()
                && let Some(last) = words.last_mut()
            {
                close_sentence(last);
                sentences += 1;
            }
            continue;
        }
        if ends_sentence {
            sentences += 1;
        }
        words.push(strip_emphasis(token));
    }
    let mut readout = words.join(" ");
    if finished && !readout.is_empty() && !readout.ends_with(['.', '!', '?']) {
        readout.truncate(readout.trim_end_matches([':', ',', ';']).len());
        readout.push('.');
    }
    readout
}

/// The last safety filter before text is synthesized: drops every token that looks like code,
/// shell syntax, math, a path, a URL, a flag, or a long number, keeping only plain words and
/// sentence punctuation. Returns an empty string when nothing speakable remains.
pub fn plain_speech(text: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for token in strip_inline_code(text).split_whitespace() {
        let core = token.trim_matches(|c: char| !c.is_alphanumeric());
        if core.is_empty() || is_technical(token, core) {
            if token.ends_with(['.', '!', '?'])
                && !core.is_empty()
                && let Some(last) = words.last_mut()
            {
                close_sentence(last);
            }
            continue;
        }
        words.push(strip_emphasis(token));
    }
    let spoken = words.join(" ");
    if spoken.chars().any(char::is_alphabetic) {
        spoken
    } else {
        String::new()
    }
}

fn close_sentence(word: &mut String) {
    if !word.ends_with(['.', '!', '?']) {
        word.truncate(word.trim_end_matches([':', ',', ';']).len());
        word.push('.');
    }
}

fn is_list_or_table_line(line: &str) -> bool {
    if line.starts_with('|') || ["- ", "* ", "+ "].iter().any(|m| line.starts_with(m)) {
        return true;
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && (line[digits..].starts_with(". ") || line[digits..].starts_with(") "))
}

fn strip_inline_code(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_code = false;
    for c in line.chars() {
        if c == '`' {
            in_code = !in_code;
        } else if !in_code {
            out.push(c);
        }
    }
    out
}

fn strip_emphasis(token: &str) -> String {
    token
        .chars()
        .filter(|c| !matches!(c, '*' | '`' | '[' | ']'))
        .collect()
}

/// Whether a word is technical data that makes no sense read aloud.
fn is_technical(token: &str, core: &str) -> bool {
    let has_digit = core.chars().any(|c| c.is_ascii_digit());
    let has_alpha = core.chars().any(|c| c.is_ascii_alphabetic());
    let start = token.trim_start_matches(['(', '"', '\'']);
    token.contains("://")
        || token.contains([
            '*', '|', '`', '$', '=', '/', '\\', '<', '>', '{', '}', '[', ']',
        ])
        || token.contains("((")
        || token.contains("))")
        || (core.chars().count() == 1
            && core.chars().all(|c| c.is_ascii_alphabetic())
            && !matches!(core, "a" | "A" | "I"))
        || token.starts_with("www.")
        || core.contains(['/', '\\', '=', '_', '<', '>', '{', '}', '(', ')', '$', ':'])
        || start.starts_with(['-', '/', '~', '.', '$'])
        || core.contains('.')
        || (core.len() >= 7
            && has_digit
            && has_alpha
            && core.chars().all(|c| c.is_ascii_hexdigit()))
        || core.chars().filter(char::is_ascii_digit).count() > MAX_SPOKEN_DIGITS
}

#[cfg(test)]
#[path = "spoken_tests.rs"]
mod tests;
