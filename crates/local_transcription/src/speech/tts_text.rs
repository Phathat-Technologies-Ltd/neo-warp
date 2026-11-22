//! Rewrites technical tokens so a speech engine reads them naturally: `--release` becomes "dash
//! dash release", `app/src/lib.rs` becomes "app slash src slash lib dot rs", and a commit SHA
//! becomes "commit" and its first seven characters.

/// Paths with more components than this are shortened to their last component.
const MAX_SPOKEN_PATH_COMPONENTS: usize = 3;

/// Acronyms that are said as words rather than spelled out.
const WORD_ACRONYMS: &[&str] = &[
    "ASCII", "GIF", "JPEG", "JSON", "NASA", "OAUTH", "PNG", "README", "REST", "SCSI", "TOML",
    "WASM", "YAML",
];

/// Punctuation that ends or splits a sentence and is kept for the engine's prosody.
fn is_prosody_mark(c: char) -> bool {
    matches!(c, ',' | '.' | '?' | '!' | ':' | ';')
}

/// Rewrites `text` for speech.
pub fn normalize(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split_whitespace() {
        let (core, trailing) = split_trailing_marks(raw);
        let core = core.trim_matches(|c: char| matches!(c, '`' | '*' | '"' | '\'' | '(' | ')'));
        let spoken = speak_token(core);
        match (spoken.is_empty(), out.last_mut()) {
            (true, Some(last)) if !trailing.is_empty() => last.push_str(trailing),
            (true, _) => {}
            (false, _) => out.push(format!("{spoken}{trailing}")),
        }
    }
    out.join(" ")
}

fn split_trailing_marks(token: &str) -> (&str, &str) {
    let core = token.trim_end_matches(is_prosody_mark);
    // A token made only of marks, or a trailing "." that is part of a path like "..", stays.
    if core.is_empty() || core.ends_with(['.', '/']) {
        return (token, "");
    }
    (core, &token[core.len()..])
}

fn speak_token(token: &str) -> String {
    if token.is_empty() {
        return String::new();
    }
    if let Some(spoken) = speak_symbol(token) {
        return spoken.to_owned();
    }
    if let Some(rest) = token.strip_prefix("--")
        && rest.chars().next().is_some_and(char::is_alphanumeric)
    {
        return format!("dash dash {}", speak_flag_body(rest));
    }
    if let Some(rest) = token.strip_prefix('-') {
        if rest.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() {
            return format!("minus {rest}");
        }
        if !rest.is_empty() && rest.len() <= 4 && rest.chars().all(|c| c.is_ascii_alphabetic()) {
            return format!("dash {}", spell(rest));
        }
    }
    if let Some(rest) = token
        .strip_prefix("https://")
        .or_else(|| token.strip_prefix("http://"))
    {
        let host = rest.split('/').next().unwrap_or(rest);
        return format!("a link to {}", speak_dotted(host));
    }
    if is_path(token) {
        return speak_path(token);
    }
    if is_sha(token) {
        return format!("commit {}", spell(&token[..7]));
    }
    speak_word(token)
}

fn speak_symbol(token: &str) -> Option<&'static str> {
    Some(match token {
        "&&" => "and",
        "||" => "or",
        "|" => "pipe",
        "=" | "==" => "equals",
        "!=" => "not equals",
        ">" => "greater than",
        "<" => "less than",
        "~" => "home",
        "->" | "=>" | "-" | "—" | "–" | "#" | "##" | "###" | "*" | "**" | "`" | "```" | "..." => {
            ""
        }
        _ => return None,
    })
}

fn speak_flag_body(body: &str) -> String {
    match body.split_once('=') {
        Some((name, value)) => format!(
            "{} equals {}",
            name.replace(['-', '_'], " "),
            speak_token(value)
        ),
        None => body.replace(['-', '_'], " "),
    }
}

fn is_path(token: &str) -> bool {
    token.starts_with("~/")
        || token.starts_with("./")
        || token.starts_with("../")
        || (token.contains('/') && token.chars().any(char::is_alphanumeric))
}

fn speak_path(token: &str) -> String {
    let (prefix, rest) = if let Some(rest) = token.strip_prefix("~/") {
        ("home slash ", rest)
    } else if let Some(rest) = token.strip_prefix("./") {
        ("", rest)
    } else if let Some(rest) = token.strip_prefix('/') {
        ("slash ", rest)
    } else {
        ("", token)
    };
    let components: Vec<&str> = rest.split('/').filter(|part| !part.is_empty()).collect();
    if components.len() > MAX_SPOKEN_PATH_COMPONENTS {
        return components
            .last()
            .map(|last| speak_dotted(last))
            .unwrap_or_default();
    }
    let spoken: Vec<String> = components
        .iter()
        .map(|part| match *part {
            ".." => "up".to_owned(),
            part => speak_dotted(part),
        })
        .collect();
    format!("{prefix}{}", spoken.join(" slash "))
}

/// Speaks a name like `lib.rs` or `github.com`, splitting it on dots.
fn speak_dotted(name: &str) -> String {
    name.split('.')
        .filter(|part| !part.is_empty())
        .map(speak_word)
        .collect::<Vec<_>>()
        .join(" dot ")
}

fn is_sha(token: &str) -> bool {
    (7..=40).contains(&token.len())
        && token.chars().all(|c| c.is_ascii_hexdigit())
        && token.chars().any(|c| c.is_ascii_digit())
        && token.chars().any(|c| c.is_ascii_alphabetic())
}

fn spell(letters: &str) -> String {
    letters
        .chars()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Speaks a single word, splitting identifiers and spelling out acronyms.
fn speak_word(word: &str) -> String {
    if word.contains('.') && word.chars().any(char::is_alphabetic) && !word.starts_with('.') {
        return speak_dotted(word);
    }
    let parts = split_identifier(word);
    parts
        .iter()
        .map(|part| {
            let is_acronym =
                part.len() >= 2 && part.len() <= 6 && part.chars().all(|c| c.is_ascii_uppercase());
            if is_acronym && !WORD_ACRONYMS.contains(&part.as_str()) {
                spell(part)
            } else {
                part.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Splits `snake_case`, `kebab-case`, and `camelCase` identifiers into words; plain words come
/// back whole.
fn split_identifier(word: &str) -> Vec<String> {
    let mut parts = Vec::new();
    for piece in word.split(['_', '-']).filter(|piece| !piece.is_empty()) {
        let chars: Vec<char> = piece.chars().collect();
        let mut current = String::new();
        for (index, &c) in chars.iter().enumerate() {
            let previous = index.checked_sub(1).map(|i| chars[i]);
            let next = chars.get(index + 1).copied();
            let starts_word = c.is_uppercase()
                && (previous.is_some_and(char::is_lowercase)
                    || (previous.is_some_and(char::is_uppercase)
                        && next.is_some_and(char::is_lowercase)));
            if starts_word && !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            current.push(c);
        }
        if !current.is_empty() {
            parts.push(current);
        }
    }
    parts
}

#[cfg(test)]
#[path = "tts_text_tests.rs"]
mod tests;
