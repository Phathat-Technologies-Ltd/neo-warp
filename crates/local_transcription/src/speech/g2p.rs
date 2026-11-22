//! English text to phonemes without espeak-ng (GPL): words are looked up in CMUdict (BSD-style
//! license), a terminal lexicon covers words CMUdict lacks, and letter-to-sound rules handle the
//! rest. The output is ARPAbet, which each engine maps to its own phoneme set.

use std::collections::HashMap;
use std::path::Path;

use super::SpeakError;

/// The CMUdict file each speech output model bundles.
pub const CMUDICT_FILE: &str = "cmudict.dict";

/// One ARPAbet phone. Vowels carry a stress of 0 (none), 1 (primary), or 2 (secondary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phone {
    pub symbol: String,
    pub stress: Option<u8>,
}

impl Phone {
    fn parse(token: &str) -> Self {
        match token.char_indices().last() {
            Some((index, digit @ '0'..='2')) => Self {
                symbol: token[..index].to_owned(),
                stress: Some(digit as u8 - b'0'),
            },
            _ => Self {
                symbol: token.to_owned(),
                stress: None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum G2pToken {
    Word(Vec<Phone>),
    /// Punctuation that shapes prosody: `, . ? ! : ;`.
    Punct(char),
}

/// Words CMUdict lacks or says wrongly in a terminal context.
const TERMINAL_LEXICON: &[(&str, &str)] = &[
    ("git", "G IH1 T"),
    ("sudo", "S UW1 D OW0"),
    ("nginx", "EH1 N JH IH0 N EH2 K S"),
    ("kubectl", "K UW1 B K AH0 N T R OW2 L"),
    ("json", "JH EY1 S AH0 N"),
    ("yaml", "Y AE1 M AH0 L"),
    ("toml", "T AA1 M AH0 L"),
    ("regex", "R EH1 JH EH2 K S"),
    ("stdout", "S T AE1 N D ER0 D AW2 T"),
    ("stdin", "S T AE1 N D ER0 D IH2 N"),
    ("stderr", "S T AE1 N D ER0 EH2 R ER0"),
    ("async", "EY1 S IH2 NG K"),
    ("enum", "IY1 N AH0 M"),
    ("repo", "R IY1 P OW0"),
    ("config", "K AA1 N F IH0 G"),
    ("env", "EH1 N V"),
    ("args", "AA1 R G Z"),
    ("bool", "B UW1 L"),
    ("struct", "S T R AH1 K T"),
    ("impl", "IH1 M P L"),
    ("rustc", "R AH1 S T S IY1"),
    ("readme", "R IY1 D M IY0"),
    ("wasm", "W AA1 Z AH0 M"),
    ("oauth", "OW1 AO2 TH"),
    ("localhost", "L OW1 K AH0 L HH OW2 S T"),
    ("dev", "D EH1 V"),
    ("src", "S AO1 R S"),
    ("lib", "L IH1 B"),
    ("rs", "AA1 R EH1 S"),
    ("py", "P AY1"),
    ("ts", "T IY1 EH1 S"),
    ("js", "JH EY1 EH1 S"),
    ("md", "EH1 M D IY1"),
];

/// Turns words into phones.
#[derive(Debug, Default)]
pub struct G2p {
    dictionary: HashMap<String, Vec<Phone>>,
}

impl G2p {
    /// Loads CMUdict from `dir`.
    pub fn load(dir: &Path) -> Result<Self, SpeakError> {
        let text = std::fs::read_to_string(dir.join(CMUDICT_FILE))
            .map_err(|error| SpeakError::Engine(format!("couldn't read CMUdict: {error}")))?;
        Ok(Self::from_cmudict(&text))
    }

    /// Parses CMUdict text, keeping each word's first pronunciation.
    pub fn from_cmudict(text: &str) -> Self {
        let mut dictionary = HashMap::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or_default();
            let mut parts = line.split_whitespace();
            let Some(word) = parts.next() else { continue };
            if word.ends_with(')') {
                continue;
            }
            let phones: Vec<Phone> = parts.map(Phone::parse).collect();
            if !phones.is_empty() {
                dictionary.entry(word.to_lowercase()).or_insert(phones);
            }
        }
        for (word, phones) in TERMINAL_LEXICON {
            dictionary.insert(
                (*word).to_owned(),
                phones.split_whitespace().map(Phone::parse).collect(),
            );
        }
        Self { dictionary }
    }

    /// Converts normalized text (see [`super::tts_text::normalize`]) to phones.
    pub fn convert(&self, text: &str) -> Vec<G2pToken> {
        let mut tokens = Vec::new();
        for raw in text.split_whitespace() {
            let mut word = String::new();
            let mut trailing = Vec::new();
            for c in raw.chars() {
                if matches!(c, ',' | '.' | '?' | '!' | ':' | ';') {
                    trailing.push(c);
                } else if c.is_alphanumeric() || c == '\'' {
                    if !trailing.is_empty() {
                        // A mark inside a token, as in "1.5", splits it.
                        self.push_word(&std::mem::take(&mut word), &mut tokens);
                        if trailing == ['.'] && c.is_ascii_digit() {
                            tokens.extend(self.word_phones("point").map(G2pToken::Word));
                        }
                        trailing.clear();
                    }
                    word.push(c);
                }
            }
            self.push_word(&word, &mut tokens);
            if let Some(&mark) = trailing.first() {
                tokens.push(G2pToken::Punct(mark));
            }
        }
        tokens
    }

    fn push_word(&self, word: &str, tokens: &mut Vec<G2pToken>) {
        if word.is_empty() {
            return;
        }
        if word.chars().all(|c| c.is_ascii_digit()) {
            for spoken in number_words(word).split_whitespace() {
                tokens.extend(self.word_phones(spoken).map(G2pToken::Word));
            }
            return;
        }
        tokens.extend(self.word_phones(word).map(G2pToken::Word));
    }

    fn word_phones(&self, word: &str) -> Option<Vec<Phone>> {
        let key = word.to_lowercase();
        if let Some(phones) = self.dictionary.get(&key) {
            return Some(phones.clone());
        }
        let has_vowel = key.chars().any(|c| "aeiouy".contains(c));
        if (key.len() <= 3 && !has_vowel) || key.chars().count() == 1 {
            // Spell out consonant clusters such as "ls" or "cd".
            let mut phones = Vec::new();
            for letter in key.chars() {
                phones.extend(self.dictionary.get(&letter.to_string()).cloned()?);
            }
            return Some(phones);
        }
        let phones = letter_to_sound(&key);
        (!phones.is_empty()).then_some(phones)
    }
}

/// Spells out a whole number up to 999,999 in words, and longer ones digit by digit.
pub fn number_words(digits: &str) -> String {
    const ONES: [&str; 20] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ];
    const TENS: [&str; 10] = [
        "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    fn below_thousand(n: u32) -> String {
        let mut parts = Vec::new();
        if n >= 100 {
            parts.push(format!("{} hundred", ONES[(n / 100) as usize]));
        }
        let rest = n % 100;
        if rest >= 20 {
            let tens = TENS[(rest / 10) as usize];
            match rest % 10 {
                0 => parts.push(tens.to_owned()),
                ones => parts.push(format!("{tens} {}", ONES[ones as usize])),
            }
        } else if rest > 0 || parts.is_empty() {
            parts.push(ONES[rest as usize].to_owned());
        }
        parts.join(" ")
    }
    let fits = digits.len() <= 6 && !(digits.len() > 1 && digits.starts_with('0'));
    match digits.parse::<u32>() {
        Ok(n) if fits => {
            if n >= 1000 {
                let thousands = below_thousand(n / 1000);
                match n % 1000 {
                    0 => format!("{thousands} thousand"),
                    rest => format!("{thousands} thousand {}", below_thousand(rest)),
                }
            } else {
                below_thousand(n)
            }
        }
        _ => digits
            .chars()
            .filter_map(|c| c.to_digit(10))
            .map(|d| ONES[d as usize])
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// A rough letter-to-sound fallback for words in no dictionary; the first vowel is stressed.
pub fn letter_to_sound(word: &str) -> Vec<Phone> {
    const RULES: &[(&str, &str)] = &[
        ("tch", "CH"),
        ("sch", "S K"),
        ("igh", "AY"),
        ("ch", "CH"),
        ("sh", "SH"),
        ("th", "TH"),
        ("ph", "F"),
        ("wh", "W"),
        ("ck", "K"),
        ("ng", "NG"),
        ("qu", "K W"),
        ("ee", "IY"),
        ("ea", "IY"),
        ("oo", "UW"),
        ("ou", "AW"),
        ("ow", "OW"),
        ("oi", "OY"),
        ("oy", "OY"),
        ("ai", "EY"),
        ("ay", "EY"),
        ("au", "AO"),
        ("aw", "AO"),
        ("er", "ER"),
        ("ar", "AA R"),
        ("or", "AO R"),
        ("a", "AE"),
        ("b", "B"),
        ("c", "K"),
        ("d", "D"),
        ("e", "EH"),
        ("f", "F"),
        ("g", "G"),
        ("h", "HH"),
        ("i", "IH"),
        ("j", "JH"),
        ("k", "K"),
        ("l", "L"),
        ("m", "M"),
        ("n", "N"),
        ("o", "AA"),
        ("p", "P"),
        ("q", "K"),
        ("r", "R"),
        ("s", "S"),
        ("t", "T"),
        ("u", "AH"),
        ("v", "V"),
        ("w", "W"),
        ("x", "K S"),
        ("y", "IY"),
        ("z", "Z"),
    ];
    const VOWELS: &[&str] = &[
        "AA", "AE", "AH", "AO", "AW", "AY", "EH", "ER", "EY", "IH", "IY", "OW", "OY", "UH", "UW",
    ];
    // A final silent "e" is dropped ("make" is M EY K, roughly).
    let word = match word.strip_suffix('e') {
        Some(stem) if stem.len() > 2 => stem,
        _ => word,
    };
    let mut phones = Vec::new();
    let mut rest = word;
    let mut stressed = false;
    while !rest.is_empty() {
        let Some((graphemes, sounds)) = RULES.iter().find(|(g, _)| rest.starts_with(g)) else {
            let skip = rest.chars().next().map_or(1, char::len_utf8);
            rest = &rest[skip..];
            continue;
        };
        rest = &rest[graphemes.len()..];
        for sound in sounds.split_whitespace() {
            let stress = VOWELS.contains(&sound).then(|| {
                let stress = if stressed { 0 } else { 1 };
                stressed = true;
                stress
            });
            phones.push(Phone {
                symbol: sound.to_owned(),
                stress,
            });
        }
    }
    phones
}

#[cfg(test)]
#[path = "g2p_tests.rs"]
mod tests;
