//! Spoken approval of commands the agent wants to run.

use std::time::Duration;

use instant::Instant;

/// How long after the assistant's audio stops its echo may still reach the microphone, so speech
/// that starts within it is not trusted as the user's.
pub const ECHO_TAIL: Duration = Duration::from_millis(700);

/// How long a spoken answer is accepted once the prompt has played; after that only the on-screen
/// buttons answer.
pub const APPROVAL_LISTEN_FOR: Duration = Duration::from_secs(20);

/// How long after one part of a spoken answer the next utterance still counts as its rest. Short
/// answers often reach the recognizer as two utterances ("run" then "it"), or lose their first
/// word to the voice activity detector.
pub const ANSWER_JOIN_WINDOW: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalReply {
    Approve,
    Reject,
}

const APPROVE: &[&[&str]] = &[
    &["yes"],
    &["yeah"],
    &["yep"],
    &["sure"],
    &["approve"],
    &["approved"],
    &["run"],
    &["run", "it"],
    &["yes", "run"],
    &["yes", "run", "it"],
    &["okay", "run"],
    &["okay", "run", "it"],
    &["ok", "run"],
    &["ok", "run", "it"],
    &["sure", "run", "it"],
    &["go"],
    &["go", "ahead"],
    &["yes", "go", "ahead"],
    &["okay", "go", "ahead"],
    &["ok", "go", "ahead"],
    &["do", "it"],
    &["yes", "do", "it"],
];
const REJECT: &[&[&str]] = &[
    &["no"],
    &["nope"],
    &["nah"],
    &["skip"],
    &["skip", "it"],
    &["cancel"],
    &["cancel", "it"],
    &["reject"],
    &["stop"],
    &["stop", "it"],
    &["don't"],
    &["do", "not"],
    &["don't", "run", "it"],
    &["don't", "do", "it"],
    &["no", "don't"],
    &["no", "don't", "run", "it"],
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

/// The words of `text` as an answer: lowercase, without punctuation, a leading "please", or a
/// trailing "please"/"thanks".
fn answer_words(text: &str) -> Vec<String> {
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
    words
}

fn phrases() -> impl Iterator<Item = &'static [&'static str]> {
    APPROVE.iter().chain(REJECT).copied()
}

fn match_words(words: &[String]) -> Option<ApprovalReply> {
    let matches = |phrases: &[&[&str]]| phrases.iter().any(|phrase| words == *phrase);
    if matches(APPROVE) {
        Some(ApprovalReply::Approve)
    } else if matches(REJECT) {
        Some(ApprovalReply::Reject)
    } else {
        None
    }
}

/// Whether `words` begin an answer phrase without being one ("do", "okay").
fn is_answer_prefix(words: &[String]) -> bool {
    !words.is_empty()
        && match_words(words).is_none()
        && phrases().any(|phrase| {
            phrase.len() > words.len() && phrase.iter().zip(words).all(|(a, b)| a == b)
        })
}

/// Whether `words` end an answer phrase without being one ("it", "ahead").
fn is_answer_suffix(words: &[String]) -> bool {
    !words.is_empty()
        && match_words(words).is_none()
        && phrases().any(|phrase| {
            phrase.len() > words.len()
                && phrase
                    .iter()
                    .rev()
                    .zip(words.iter().rev())
                    .all(|(a, b)| a == b)
        })
}

/// Matches a whole utterance against the approval phrases, ignoring case, punctuation, and a
/// leading "please" or trailing "please"/"thanks". Anything else is not an answer.
pub fn match_approval(text: &str) -> Option<ApprovalReply> {
    match_words(&answer_words(text))
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
    /// The start of an answer ("do"); it is dropped and joined with the next utterance.
    Held,
    /// The end of an answer whose start was lost ("it"), heard while a spoken answer is accepted.
    /// It is dropped, and the question should be asked again.
    Unclear,
    Answered(Id, ApprovalReply),
}

/// Tracks the approval the agent is waiting on and turns final utterances into answers.
#[derive(Debug, Clone)]
pub struct ApprovalFlow<Id> {
    pending: Option<PendingApproval<Id>>,
    /// The start of an answer waiting for the rest.
    held: Option<HeldWords>,
    /// The latest spoken answer, so a late rest of it is dropped rather than sent as a prompt.
    last_answer: Option<HeldWords>,
}

/// Words of a spoken answer and when they were heard.
#[derive(Debug, Clone)]
struct HeldWords {
    words: Vec<String>,
    started_at: Option<Instant>,
    heard_at: Instant,
}

impl HeldWords {
    fn is_recent(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.heard_at) <= ANSWER_JOIN_WINDOW
    }
}

impl<Id> Default for ApprovalFlow<Id> {
    fn default() -> Self {
        Self {
            pending: None,
            held: None,
            last_answer: None,
        }
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
        self.held = None;
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

    /// The prompt was cut off at `now` by the user talking over it from `talking_from`. That speech
    /// is the user's, so it and anything after it may answer.
    pub fn prompt_interrupted(&mut self, talking_from: Instant, now: Instant) {
        if let Some(pending) = &mut self.pending {
            pending.listening_from = Some(talking_from.min(now));
            pending.listening_until = Some(now + APPROVAL_LISTEN_FOR);
        }
    }

    /// Whether a spoken answer to speech that started at `started_at` would be accepted at `now`.
    pub fn accepts_answer(&self, started_at: Option<Instant>, now: Instant) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| Self::in_window(pending, started_at, now))
    }

    fn in_window(pending: &PendingApproval<Id>, started_at: Option<Instant>, now: Instant) -> bool {
        let in_window = match (pending.listening_from, pending.listening_until, started_at) {
            (Some(from), Some(until), Some(started)) => started >= from && now < until,
            _ => false,
        };
        !pending.needs_screen && in_window
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
        self.held = None;
        self.last_answer = None;
    }

    /// Answers the pending approval from a final utterance whose speech started at `started_at`.
    /// Only a whole approval phrase whose speech started after the prompt played, within the
    /// answer window, answers it. An answer split across utterances is joined within
    /// [`ANSWER_JOIN_WINDOW`], and parts of an answer never count as a prompt.
    pub fn on_utterance(
        &mut self,
        text: &str,
        started_at: Option<Instant>,
        now: Instant,
    ) -> ApprovalVerdict<Id> {
        let words = answer_words(text);
        if let Some(last) = self.last_answer.take().filter(|last| last.is_recent(now)) {
            let joined = [last.words.as_slice(), words.as_slice()].concat();
            if !words.is_empty() && (match_words(&joined).is_some() || is_answer_suffix(&words)) {
                return ApprovalVerdict::Ignored;
            }
        }
        let Some(pending) = self.pending.as_ref() else {
            return ApprovalVerdict::NotAnAnswer;
        };
        let held = self.held.take().filter(|held| held.is_recent(now));
        let (words, started_at) = match held {
            Some(held) => {
                let joined = [held.words.as_slice(), words.as_slice()].concat();
                if match_words(&joined).is_some() || is_answer_prefix(&joined) {
                    (joined, held.started_at)
                } else {
                    (words, started_at)
                }
            }
            None => (words, started_at),
        };
        let in_window = Self::in_window(pending, started_at, now);
        if let Some(reply) = match_words(&words) {
            if !in_window {
                return ApprovalVerdict::Ignored;
            }
            return match self.pending.take() {
                Some(pending) => {
                    self.last_answer = Some(HeldWords {
                        words,
                        started_at,
                        heard_at: now,
                    });
                    ApprovalVerdict::Answered(pending.id, reply)
                }
                None => ApprovalVerdict::NotAnAnswer,
            };
        }
        if is_answer_prefix(&words) {
            self.held = Some(HeldWords {
                words,
                started_at,
                heard_at: now,
            });
            return ApprovalVerdict::Held;
        }
        if is_answer_suffix(&words) {
            return if in_window {
                ApprovalVerdict::Unclear
            } else {
                ApprovalVerdict::Ignored
            };
        }
        ApprovalVerdict::NotAnAnswer
    }
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod tests;
