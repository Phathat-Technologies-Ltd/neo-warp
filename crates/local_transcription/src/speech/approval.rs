//! Spoken approval of commands the agent wants to run.

use super::tts_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalReply {
    Approve,
    Reject,
}

const APPROVE: &[&[&str]] = &[
    &["yes"],
    &["yeah"],
    &["yep"],
    &["approve"],
    &["approved"],
    &["run", "it"],
    &["yes", "run", "it"],
    &["go", "ahead"],
];
const REJECT: &[&[&str]] = &[
    &["no"],
    &["nope"],
    &["skip"],
    &["skip", "it"],
    &["cancel"],
    &["reject"],
    &["don't"],
    &["do", "not"],
];

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// Matches a whole utterance against the approval phrases, ignoring case, punctuation, and a
/// leading "please" or trailing "please"/"thanks". Anything else is not an answer.
pub fn match_approval(text: &str) -> Option<ApprovalReply> {
    let mut words = words(text);
    if words.first().is_some_and(|word| word == "please") {
        words.remove(0);
    }
    while words
        .last()
        .is_some_and(|word| matches!(word.as_str(), "please" | "thanks"))
    {
        words.pop();
    }
    let matches = |phrases: &[&[&str]]| phrases.iter().any(|phrase| words == *phrase);
    if matches(APPROVE) {
        Some(ApprovalReply::Approve)
    } else if matches(REJECT) {
        Some(ApprovalReply::Reject)
    } else {
        None
    }
}

/// Whether the utterance asks to stop the speech.
pub fn is_stop_phrase(text: &str) -> bool {
    let words = words(text);
    matches!(
        words
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["stop"] | ["stop", "talking"] | ["stop", "it"] | ["be", "quiet"] | ["quiet"] | ["shush"]
    )
}

/// Commands longer than this many words are read as their first words.
const MAX_SPOKEN_COMMAND_WORDS: usize = 6;

/// The prompt spoken when the agent asks to run `command`. Commands that need on-screen approval
/// are announced without asking for a spoken answer.
pub fn approval_prompt(command: &str, needs_screen: bool) -> String {
    let words: Vec<&str> = command.split_whitespace().collect();
    let short = if words.len() > MAX_SPOKEN_COMMAND_WORDS {
        format!("{} and so on", words[..MAX_SPOKEN_COMMAND_WORDS].join(" "))
    } else {
        words.join(" ")
    };
    let short = tts_text::normalize(&short);
    if needs_screen {
        format!("I want to run {short}. Please approve it on screen.")
    } else {
        format!("I want to run {short}. Say yes or no.")
    }
}

/// An approval waiting for a spoken answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval<Id> {
    pub id: Id,
    /// Set for commands the risk classification marks as destructive, which only the on-screen
    /// buttons approve.
    pub needs_screen: bool,
}

/// Tracks the approval the agent is waiting on and turns final utterances into answers.
#[derive(Debug, Clone)]
pub struct ApprovalFlow<Id> {
    pending: Option<PendingApproval<Id>>,
}

impl<Id> Default for ApprovalFlow<Id> {
    fn default() -> Self {
        Self { pending: None }
    }
}

impl<Id: Clone + PartialEq> ApprovalFlow<Id> {
    /// Records that the agent waits on `id`, returning the prompt to speak.
    pub fn request(&mut self, id: Id, command: &str, needs_screen: bool) -> String {
        self.pending = Some(PendingApproval { id, needs_screen });
        approval_prompt(command, needs_screen)
    }

    pub fn pending(&self) -> Option<&PendingApproval<Id>> {
        self.pending.as_ref()
    }

    /// The approval was answered elsewhere (on screen) or went away.
    pub fn resolved(&mut self, id: &Id) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| &pending.id == id)
        {
            self.pending = None;
        }
    }

    pub fn clear(&mut self) {
        self.pending = None;
    }

    /// Answers the pending approval from a final utterance. Returns `None`, keeping the approval
    /// pending, when the utterance isn't an answer or the command needs on-screen approval.
    pub fn on_utterance(&mut self, text: &str) -> Option<(Id, ApprovalReply)> {
        let pending = self.pending.as_ref()?;
        if pending.needs_screen {
            return None;
        }
        let reply = match_approval(text)?;
        let pending = self.pending.take()?;
        Some((pending.id, reply))
    }
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod tests;
