/// A command spoken at the end of an utterance in hands-free mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceCommand {
    /// Sends the prompt.
    Send,
    /// Clears the input.
    Cancel,
    /// Inserts a newline.
    NewLine,
}

/// Spoken phrases, longest first so "send it" wins over "send".
const PHRASES: &[(&[&str], VoiceCommand)] = &[
    (&["scratch", "that"], VoiceCommand::Cancel),
    (&["send", "it"], VoiceCommand::Send),
    (&["new", "line"], VoiceCommand::NewLine),
    (&["newline"], VoiceCommand::NewLine),
    (&["send"], VoiceCommand::Send),
    (&["cancel"], VoiceCommand::Cancel),
];

fn key(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Splits a trailing voice command off a final transcript. Command words count only as the last
/// word or words; the rest of the text is returned without them and without the punctuation
/// that preceded them.
pub fn split_voice_command(text: &str) -> (String, Option<VoiceCommand>) {
    let words: Vec<&str> = text.split_whitespace().collect();
    let keys: Vec<String> = words.iter().map(|word| key(word)).collect();
    for (phrase, command) in PHRASES {
        if keys.len() >= phrase.len()
            && keys[keys.len() - phrase.len()..]
                .iter()
                .zip(phrase.iter())
                .all(|(key, word)| key == word)
        {
            let rest = words[..words.len() - phrase.len()].join(" ");
            let rest = rest.trim_end_matches([',', '.', ';', ':']).to_owned();
            return (rest, Some(*command));
        }
    }
    (text.trim().to_owned(), None)
}

#[cfg(test)]
#[path = "voice_command_tests.rs"]
mod tests;
