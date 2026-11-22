//! Spoken approval of commands the agent wants to run.

use std::time::Duration;

use instant::Instant;

/// How long after the assistant's audio stops its echo may still reach the microphone, so speech
/// that starts within it is not trusted as the user's.
pub const ECHO_TAIL: Duration = Duration::from_millis(700);

/// How long a spoken answer is accepted once the prompt has played; after that only the on-screen
/// buttons answer.
pub const APPROVAL_LISTEN_FOR: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalReply {
    Approve,
    Reject,
}

const APPROVE: &[&[&str]] = &[
    &["yes"],
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
        ["stop"]
            | ["stop", "talking"]
            | ["stop", "it"]
            | ["be", "quiet"]
            | ["quiet"]
            | ["shush"]
            | ["enough"]
            | ["that's", "enough"]
    )
}

/// The prompt spoken when the agent asks to do `subject` (a plain description such as "run a
/// command that deletes files", never raw command text). Actions that need on-screen approval are
/// announced without asking for a spoken answer.
pub fn approval_prompt(subject: &str, needs_screen: bool) -> String {
    if needs_screen {
        format!("I want to {subject}. Approve on screen.")
    } else {
        format!("I want to {subject}. Say run it, or no.")
    }
}

/// An approval waiting for a spoken answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval<Id> {
    pub id: Id,
    /// Set for commands that only the on-screen buttons approve: ones the risk classification
    /// marks as destructive or leaves unclassified, and ones whose spoken answer window passed.
    pub needs_screen: bool,
    /// Spoken answers count only for speech that starts at or after this, once the prompt has
    /// played and its echo has died down. `None` until the prompt has played.
    pub listening_from: Option<Instant>,
    pub listening_until: Option<Instant>,
}

/// What a final utterance meant to the pending approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalVerdict<Id> {
    /// Not an approval phrase, or no approval is pending; the utterance is a normal prompt.
    NotAnAnswer,
    /// An approval phrase that can't answer this approval (said before the prompt finished, after
    /// the answer window, or for a command that needs the screen). It is dropped so it never
    /// reaches the agent as a prompt.
    Ignored,
    Answered(Id, ApprovalReply),
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
    pub fn request(&mut self, id: Id, subject: &str, needs_screen: bool) -> String {
        self.pending = Some(PendingApproval {
            id,
            needs_screen,
            listening_from: None,
            listening_until: None,
        });
        approval_prompt(subject, needs_screen)
    }

    pub fn pending(&self) -> Option<&PendingApproval<Id>> {
        self.pending.as_ref()
    }

    /// The prompt finished playing (or could not be played) at `now`.
    pub fn prompt_played(&mut self, now: Instant) {
        if let Some(pending) = &mut self.pending {
            let from = now + ECHO_TAIL;
            pending.listening_from = Some(from);
            pending.listening_until = Some(from + APPROVAL_LISTEN_FOR);
        }
    }

    /// Stops accepting spoken answers once the answer window has passed. Returns whether it just
    /// closed.
    pub fn check_timeout(&mut self, now: Instant) -> bool {
        let Some(pending) = &mut self.pending else {
            return false;
        };
        if pending.needs_screen || !pending.listening_until.is_some_and(|until| now >= until) {
            return false;
        }
        pending.needs_screen = true;
        true
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

    /// Answers the pending approval from a final utterance whose speech started at `started_at`.
    /// Only a whole approval phrase whose speech started after the prompt played, within the
    /// answer window, answers it.
    pub fn on_utterance(
        &mut self,
        text: &str,
        started_at: Option<Instant>,
        now: Instant,
    ) -> ApprovalVerdict<Id> {
        let Some(pending) = self.pending.as_ref() else {
            return ApprovalVerdict::NotAnAnswer;
        };
        let Some(reply) = match_approval(text) else {
            return ApprovalVerdict::NotAnAnswer;
        };
        let in_window = match (pending.listening_from, pending.listening_until, started_at) {
            (Some(from), Some(until), Some(started)) => started >= from && now < until,
            _ => false,
        };
        if pending.needs_screen || !in_window {
            return ApprovalVerdict::Ignored;
        }
        match self.pending.take() {
            Some(pending) => ApprovalVerdict::Answered(pending.id, reply),
            None => ApprovalVerdict::NotAnAnswer,
        }
    }
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod tests;
