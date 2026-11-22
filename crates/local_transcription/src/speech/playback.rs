//! The conversation-mode state machine, free of I/O so it can be tested: it turns a streamed
//! reply into a queue of sentences that are synthesized one ahead of playback, and stops the
//! speech when the user talks over it or answers an approval.

use std::collections::VecDeque;
use std::time::Duration;

use instant::Instant;

use super::approval::{ApprovalFlow, ApprovalReply, is_stop_phrase};
use super::chunker::{ChunkerOptions, SentenceChunker};

/// What the conversation indicator shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConversationState {
    #[default]
    Off,
    /// Waiting for the user to speak.
    Listening,
    /// A request was sent and nothing is being spoken yet.
    Thinking,
    Speaking,
}

impl ConversationState {
    pub fn label(self) -> &'static str {
        match self {
            ConversationState::Off => "Conversation off",
            ConversationState::Listening => "Listening",
            ConversationState::Thinking => "Thinking",
            ConversationState::Speaking => "Speaking",
        }
    }
}

/// How long the user must talk over the assistant before the speech stops, so a cough or a
/// click does not interrupt it.
pub const BARGE_IN_AFTER: Duration = Duration::from_millis(120);

/// Work the owner must carry out.
#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackAction<A> {
    /// Synthesize `text`; report the result with [`ConversationController::synthesized`].
    Synthesize { reply: u64, seq: u64, text: String },
    /// Play `audio`; report the end with [`ConversationController::playback_finished`].
    Play { reply: u64, seq: u64, audio: A },
    /// Stop the audio now playing and drop anything queued in the output device.
    StopPlayback,
    /// The user talked over reply `reply`; its block should say it was interrupted.
    Interrupted { reply: u64 },
}

/// What a final utterance meant to the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalOutcome<Id> {
    /// The utterance is a normal prompt for the input.
    NotConsumed,
    /// The user said "stop"; nothing is sent.
    Stopped,
    /// The utterance answered the pending approval.
    Answered(Id, ApprovalReply),
}

#[derive(Debug)]
pub struct ConversationController<A, Id> {
    state: ConversationState,
    chunker_options: ChunkerOptions,
    chunker: SentenceChunker,
    /// Counts replies so results for an abandoned one are dropped.
    reply: u64,
    next_seq: u64,
    queued: VecDeque<(u64, String)>,
    synthesizing: Option<u64>,
    /// Synthesized sentences waiting for the one playing to end.
    ready: VecDeque<(u64, A)>,
    playing: Option<u64>,
    reply_done: bool,
    /// When the user started talking while the assistant was speaking.
    talking_since: Option<Instant>,
    approvals: ApprovalFlow<Id>,
}

impl<A, Id> Default for ConversationController<A, Id> {
    fn default() -> Self {
        Self {
            state: ConversationState::Off,
            chunker_options: ChunkerOptions::default(),
            chunker: SentenceChunker::default(),
            reply: 0,
            next_seq: 0,
            queued: VecDeque::new(),
            synthesizing: None,
            ready: VecDeque::new(),
            playing: None,
            reply_done: true,
            talking_since: None,
            approvals: ApprovalFlow::default(),
        }
    }
}

impl<A, Id: Clone + PartialEq> ConversationController<A, Id> {
    pub fn state(&self) -> ConversationState {
        self.state
    }

    pub fn reply(&self) -> u64 {
        self.reply
    }

    pub fn approvals(&self) -> &ApprovalFlow<Id> {
        &self.approvals
    }

    pub fn start(&mut self, chunker_options: ChunkerOptions) {
        *self = Self {
            state: ConversationState::Listening,
            chunker_options,
            chunker: SentenceChunker::new(chunker_options),
            reply: self.reply + 1,
            ..Self::default()
        };
    }

    pub fn stop(&mut self) -> Vec<PlaybackAction<A>> {
        let actions = self.silence();
        let reply = self.reply + 1;
        *self = Self::default();
        self.reply = reply;
        actions
    }

    fn is_on(&self) -> bool {
        self.state != ConversationState::Off
    }

    fn is_speaking(&self) -> bool {
        self.playing.is_some() || self.synthesizing.is_some() || !self.ready.is_empty()
    }

    /// Drops all queued speech, returning the action that stops the audio when some is playing.
    fn silence(&mut self) -> Vec<PlaybackAction<A>> {
        let was_playing = self.playing.is_some();
        self.queued.clear();
        self.ready.clear();
        self.synthesizing = None;
        self.playing = None;
        self.talking_since = None;
        self.reply += 1;
        self.chunker = SentenceChunker::new(self.chunker_options);
        if was_playing {
            vec![PlaybackAction::StopPlayback]
        } else {
            Vec::new()
        }
    }

    /// A prompt was sent; the reply that follows replaces whatever is being said.
    pub fn prompt_sent(&mut self) -> Vec<PlaybackAction<A>> {
        if !self.is_on() {
            return Vec::new();
        }
        let actions = self.silence();
        self.approvals.clear();
        self.reply_done = false;
        self.state = ConversationState::Thinking;
        actions
    }

    /// Adds streamed reply text.
    pub fn reply_text(&mut self, delta: &str) -> Vec<PlaybackAction<A>> {
        if !self.is_on() || self.reply_done {
            return Vec::new();
        }
        for sentence in self.chunker.push(delta) {
            self.enqueue(sentence);
        }
        self.pump()
    }

    /// The reply finished streaming.
    pub fn reply_finished(&mut self) -> Vec<PlaybackAction<A>> {
        if !self.is_on() || self.reply_done {
            return Vec::new();
        }
        for sentence in self.chunker.finish() {
            self.enqueue(sentence);
        }
        self.reply_done = true;
        let actions = self.pump();
        self.settle();
        actions
    }

    /// Speaks `text` after anything already queued, such as a tool-call summary.
    pub fn speak(&mut self, text: String) -> Vec<PlaybackAction<A>> {
        if !self.is_on() {
            return Vec::new();
        }
        self.enqueue(text);
        self.pump()
    }

    /// The agent waits on approval `id` to run `command`; the prompt is spoken after the reply so
    /// far.
    pub fn approval_requested(
        &mut self,
        id: Id,
        command: &str,
        needs_screen: bool,
    ) -> Vec<PlaybackAction<A>> {
        if !self.is_on() {
            return Vec::new();
        }
        let prompt = self.approvals.request(id, command, needs_screen);
        for sentence in self.chunker.finish() {
            self.enqueue(sentence);
        }
        self.speak(prompt)
    }

    /// The approval was answered on screen or went away.
    pub fn approval_resolved(&mut self, id: &Id) {
        self.approvals.resolved(id);
    }

    fn enqueue(&mut self, text: String) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.queued.push_back((seq, text));
    }

    /// Starts synthesizing the next sentence when nothing is in synthesis and at most one
    /// synthesized sentence is waiting.
    fn pump(&mut self) -> Vec<PlaybackAction<A>> {
        if self.synthesizing.is_some() || !self.ready.is_empty() {
            return Vec::new();
        }
        let Some((seq, text)) = self.queued.pop_front() else {
            return Vec::new();
        };
        self.synthesizing = Some(seq);
        vec![PlaybackAction::Synthesize {
            reply: self.reply,
            seq,
            text,
        }]
    }

    /// Moves to Listening once a finished reply has been fully spoken.
    fn settle(&mut self) {
        if self.is_on() && self.reply_done && !self.is_speaking() && self.queued.is_empty() {
            self.state = ConversationState::Listening;
        }
    }

    /// Reports a synthesis result; failed sentences are skipped.
    pub fn synthesized(
        &mut self,
        reply: u64,
        seq: u64,
        audio: Option<A>,
    ) -> Vec<PlaybackAction<A>> {
        if reply != self.reply || self.synthesizing != Some(seq) {
            return Vec::new();
        }
        self.synthesizing = None;
        let mut actions = Vec::new();
        if let Some(audio) = audio {
            if self.playing.is_none() {
                self.playing = Some(seq);
                self.state = ConversationState::Speaking;
                actions.push(PlaybackAction::Play { reply, seq, audio });
            } else {
                self.ready.push_back((seq, audio));
            }
        }
        actions.extend(self.pump());
        self.settle();
        actions
    }

    /// Reports that sentence `seq` finished playing.
    pub fn playback_finished(&mut self, reply: u64, seq: u64) -> Vec<PlaybackAction<A>> {
        if reply != self.reply || self.playing != Some(seq) {
            return Vec::new();
        }
        self.playing = None;
        let mut actions = Vec::new();
        if let Some((seq, audio)) = self.ready.pop_front() {
            self.playing = Some(seq);
            actions.push(PlaybackAction::Play { reply, seq, audio });
        }
        actions.extend(self.pump());
        if self.playing.is_none() && !self.reply_done {
            self.state = ConversationState::Thinking;
        }
        self.settle();
        actions
    }

    /// The VAD heard the start of speech.
    pub fn speech_started(&mut self, now: Instant) {
        if self.is_speaking() {
            self.talking_since.get_or_insert(now);
        }
    }

    /// The VAD heard the end of speech.
    pub fn speech_ended(&mut self) {
        self.talking_since = None;
    }

    /// Stops the speech once the user has talked over it for [`BARGE_IN_AFTER`].
    pub fn check_barge_in(&mut self, now: Instant) -> Vec<PlaybackAction<A>> {
        let Some(since) = self.talking_since else {
            return Vec::new();
        };
        if !self.is_speaking() || now.saturating_duration_since(since) < BARGE_IN_AFTER {
            return Vec::new();
        }
        let reply = self.reply;
        let mut actions = self.silence();
        actions.push(PlaybackAction::Interrupted { reply });
        self.reply_done = true;
        self.state = ConversationState::Listening;
        actions
    }

    /// Interprets a final utterance: "stop" ends the speech and an approval phrase answers the
    /// pending approval; anything else goes to the input as usual.
    pub fn on_final(&mut self, text: &str) -> (FinalOutcome<Id>, Vec<PlaybackAction<A>>) {
        if !self.is_on() {
            return (FinalOutcome::NotConsumed, Vec::new());
        }
        if is_stop_phrase(text) {
            let actions = self.silence();
            self.reply_done = true;
            self.state = ConversationState::Listening;
            return (FinalOutcome::Stopped, actions);
        }
        if let Some((id, reply)) = self.approvals.on_utterance(text) {
            return (FinalOutcome::Answered(id, reply), Vec::new());
        }
        (FinalOutcome::NotConsumed, Vec::new())
    }
}

#[cfg(test)]
#[path = "playback_tests.rs"]
mod tests;
