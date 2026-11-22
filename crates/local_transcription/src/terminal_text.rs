//! Turns spoken forms of terminal syntax into the syntax itself, for example "dash dash no fail
//! fast" into `--no-fail-fast` and "tilde slash dev" into `~/dev`.

/// How a symbol joins the words around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Glue {
    /// Attaches to the next word only (`~`, `-`, `--`).
    Next,
    /// Attaches to both neighbors (`.`, `/`, `_`, `=`), except that it stays apart from a command
    /// word before it, so "cd dot dot" is `cd ..` and "ls slash tmp" is `ls /tmp`.
    Both,
    /// Stands alone (`|`, `>`, `&&`).
    Spaced,
}

/// Spoken symbols, longest phrases first so "dash dash" wins over "dash".
const SYMBOLS: &[(&[&str], &str, Glue)] = &[
    (&["dash", "dash"], "--", Glue::Next),
    (&["double", "dash"], "--", Glue::Next),
    (&["minus", "minus"], "--", Glue::Next),
    (&["and", "and"], "&&", Glue::Spaced),
    (&["greater", "than"], ">", Glue::Spaced),
    (&["less", "than"], "<", Glue::Spaced),
    (&["at", "sign"], "@", Glue::Both),
    (&["back", "slash"], "\\", Glue::Both),
    (&["dash"], "-", Glue::Next),
    (&["minus"], "-", Glue::Next),
    (&["tilde"], "~", Glue::Next),
    (&["dollar"], "$", Glue::Next),
    (&["dot"], ".", Glue::Both),
    (&["slash"], "/", Glue::Both),
    (&["backslash"], "\\", Glue::Both),
    (&["underscore"], "_", Glue::Both),
    (&["equals"], "=", Glue::Both),
    (&["colon"], ":", Glue::Both),
    (&["pipe"], "|", Glue::Spaced),
    (&["star"], "*", Glue::Both),
    (&["asterisk"], "*", Glue::Both),
];

/// Commands whose spoken form is split into words.
const JOINED_WORDS: &[(&[&str], &str)] = &[
    (&["check", "out"], "checkout"),
    (&["cherry", "pick"], "cherry-pick"),
    (&["git", "hub"], "github"),
    (&["make", "dir"], "mkdir"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Symbol(&'static str, Glue),
}

/// Lowercases `word` and strips the punctuation the speech model adds, for matching.
fn key(word: &str) -> String {
    word.trim_matches(|c: char| matches!(c, ',' | '.' | '?' | '!' | ';' | '"'))
        .to_lowercase()
}

/// Strips sentence punctuation the speech model adds, keeping punctuation inside a word
/// (`main.rs`) and leading punctuation (`.env`).
fn clean_word(word: &str) -> &str {
    word.trim_end_matches([',', '?', '!', ';'])
}

fn matches_phrase(keys: &[String], phrase: &[&str]) -> bool {
    keys.len() >= phrase.len() && phrase.iter().zip(keys).all(|(word, key)| key == word)
}

fn tokenize(text: &str) -> Vec<Token> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let keys: Vec<String> = words.iter().map(|word| key(word)).collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < words.len() {
        let rest = &keys[index..];
        if let Some((phrase, symbol, glue)) = SYMBOLS
            .iter()
            .find(|(phrase, ..)| matches_phrase(rest, phrase))
        {
            tokens.push(Token::Symbol(symbol, *glue));
            index += phrase.len();
            continue;
        }
        let after_git = index > 0 && keys[index - 1] == "git";
        if let Some((phrase, joined)) = JOINED_WORDS
            .iter()
            .find(|(phrase, _)| matches_phrase(rest, phrase) && (phrase[0] != "check" || after_git))
        {
            tokens.push(Token::Word((*joined).to_owned()));
            index += phrase.len();
            continue;
        }
        let word = clean_word(words[index]);
        if !word.is_empty() {
            tokens.push(Token::Word(word.to_owned()));
        }
        index += 1;
    }
    tokens
}

/// Lowercases a word the speech model capitalized only because it starts a sentence.
fn uncapitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) if first.is_uppercase() && chars.clone().all(|c| !c.is_uppercase()) => {
            first.to_lowercase().chain(chars).collect()
        }
        _ => word.to_owned(),
    }
}

/// Rewrites spoken terminal syntax in `text`. Text without spoken symbols comes back with only
/// sentence punctuation and sentence casing removed.
pub fn normalize(text: &str) -> String {
    let tokens = tokenize(text.trim().trim_end_matches('.'));
    let mut out = String::new();
    // Whether the next word attaches to the output without a space.
    let mut attach_next = false;
    // Inside a long flag (`--no-fail-fast`), where words join with dashes.
    let mut in_long_flag = false;
    for (index, token) in tokens.iter().enumerate() {
        match token {
            Token::Word(word) => {
                let word = if index == 0 {
                    uncapitalize(word)
                } else {
                    word.clone()
                };
                if attach_next {
                    attach_next = false;
                } else if in_long_flag {
                    out.push('-');
                } else if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&word);
            }
            Token::Symbol(symbol, glue) => {
                in_long_flag = false;
                match glue {
                    Glue::Next => {
                        if !out.is_empty() && !attach_next {
                            out.push(' ');
                        }
                        in_long_flag = *symbol == "--";
                    }
                    Glue::Both => {
                        let after_command =
                            index == 1 && matches!(tokens.first(), Some(Token::Word(_)));
                        if after_command && !attach_next {
                            out.push(' ');
                        }
                    }
                    Glue::Spaced => {
                        if !out.is_empty() {
                            out.push(' ');
                        }
                    }
                }
                out.push_str(symbol);
                match glue {
                    Glue::Next | Glue::Both => attach_next = true,
                    Glue::Spaced => {
                        out.push(' ');
                        attach_next = true;
                    }
                }
            }
        }
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
#[path = "terminal_text_tests.rs"]
mod tests;
