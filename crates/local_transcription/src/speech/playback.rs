//! The conversation-mode state machine, free of I/O so it can be tested: it turns a streamed
//! reply into a queue of sentences that are synthesized one ahead of playback, and stops the
//! speech when the user talks over it or answers an approval.

use std::collections::VecDeque;
use std::time::Duration;

use instant::Instant;

use super::approval::{ApprovalFlow, ApprovalReply, ApprovalVerdict, ECHO_TAIL, is_stop_phrase};
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

/// How long the user must talk over the assistant before the speech stops when the output is
/// headphones, so a cough or a click does not interrupt it.
pub const BARGE_IN_AFTER: Duration = Duration::from_millis(120);

/// The same on loudspeakers, where the microphone also hears the assistant: without echo
/// cancellation only sustained speech is trusted to be the user's.
pub const BARGE_IN_AFTER_ON_SPEAKERS: Duration = Duration::from_millis(600);

/// How often the owner re-checks barge-in while the user keeps talking.
pub const BARGE_IN_POLL: Duration = Duration::from_millis(40);

/// Whether an output device name looks like headphones or a headset, whose sound the microphone
/// does not pick up.
pub fn is_headphone_device_name(name: &str) -> bool {
    let name = name.to_lowercase();
    [
        "headphone",
        "headset",
        "airpods",
        "earbud",
        "earphone",
        "buds",
        "beats",
    ]
    .iter()
    .any(|needle| name.contains(needle))
}

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

/// What a final utterance meant to the conversation. Every outcome but `NotConsumed` keeps the
/// utterance out of the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalOutcome<Id> {
    /// The utterance is a normal prompt for the input.
    NotConsumed,
    /// The user said "stop"; nothing is sent.
    Stopped,
    /// The utterance answered the pending approval.
    Answered(Id, ApprovalReply),
    /// An approval phrase that could not answer the pending approval.
    Ignored,
    /// The utterance started while the assistant was audible and never barged in, so it is likely
    /// the assistant's own voice.
    Echo,
}

/// The utterance the VAD is hearing or has just heard.
#[derive(Debug, Clone, Copy)]
struct Utterance {
    started_at: Instant,
    /// The assistant was audible (or its echo tail was) when the speech started.
    overlapped: bool,
    barged_in: bool,
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
    /// Set when the user interrupted or said "stop"; later replies of the same turn stay silent.
    interrupted: bool,
    /// When the current run of audio started playing.
    playing_since: Option<Instant>,
    /// When audio last stopped playing, for the echo tail.
    played_until: Option<Instant>,
    /// Whether the user is talking right now.
    talking: bool,
    utterance: Option<Utterance>,
    headphones: bool,
    /// The sentence that is the pending approval's prompt.
    approval_prompt_seq: Option<u64>,
    approvals: ApprovalFlow<Id>,
    /// Conversation mode stays on but nothing is read aloud.
    muted: bool,
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
            interrupted: false,
            playing_since: None,
            played_until: None,
            talking: false,
            utterance: None,
            headphones: false,
            approval_prompt_seq: None,
            approvals: ApprovalFlow::default(),
            muted: false,
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

    /// Whether the user is talking, so the owner keeps re-checking barge-in.
    pub fn is_user_talking(&self) -> bool {
        self.talking
    }

    pub fn start(&mut self, chunker_options: ChunkerOptions) {
        *self = Self {
            state: ConversationState::Listening,
            chunker_options,
            chunker: SentenceChunker::new(chunker_options),
            reply: self.reply + 1,
            headphones: self.headphones,
            muted: self.muted,
            ..Self::default()
        };
    }

    pub fn stop(&mut self) -> Vec<PlaybackAction<A>> {
        let actions = self.silence(None);
        let reply = self.reply + 1;
        *self = Self::default();
        self.reply = reply;
        actions
    }

    /// Whether the output is headphones, which the microphone doesn't hear, so barge-in can be
    /// quick and speech over the assistant is trusted.
    pub fn set_headphones(&mut self, headphones: bool) {
        self.headphones = headphones;
    }

    fn is_on(&self) -> bool {
        self.state != ConversationState::Off
    }

    pub fn is_muted(&self) -> bool {
        self.muted
    }

    /// Whether a reply is being read aloud or about to be (a sentence is in synthesis or queued
    /// for the output).
    pub fn is_speaking(&self) -> bool {
        self.playing.is_some() || self.synthesizing.is_some() || !self.ready.is_empty()
    }

    fn barge_in_after(&self) -> Duration {
        if self.headphones {
            BARGE_IN_AFTER
        } else {
            BARGE_IN_AFTER_ON_SPEAKERS
        }
    }

    /// Whether the assistant is audible at `now`, or was within the echo tail.
    fn is_audible(&self, now: Instant) -> bool {
        self.playing.is_some()
            || self
                .played_until
                .is_some_and(|until| now < until + ECHO_TAIL)
    }

    /// Drops all queued speech, returning the action that stops the audio when some is playing.
    fn silence(&mut self, now: Option<Instant>) -> Vec<PlaybackAction<A>> {
        let was_playing = self.playing.is_some();
        self.queued.clear();
        self.ready.clear();
        self.synthesizing = None;
        self.playing = None;
        self.playing_since = None;
        if was_playing && let Some(now) = now {
            self.played_until = Some(now);
        }
        self.approval_prompt_seq = None;
        self.reply += 1;
        self.chunker = SentenceChunker::new(self.chunker_options);
        if was_playing {
            vec![PlaybackAction::StopPlayback]
        } else {
            Vec::new()
        }
    }

    /// The user asked to stop the readout (Esc or the Stop button). The rest of the reply being
    /// read is dropped, not resumed; the next reply of the turn and later replies are spoken.
    pub fn stop_speaking(&mut self, now: Instant) -> Vec<PlaybackAction<A>> {
        if !self.is_on() || (!self.is_speaking() && self.queued.is_empty()) {
            return Vec::new();
        }
        let actions = self.silence(Some(now));
        self.reply_done = true;
        self.state = ConversationState::Listening;
        actions
    }

    /// Mutes or unmutes the readout at `now`. Muting stops what is playing; unmuting speaks from
    /// the next sentence on.
    pub fn set_muted(&mut self, muted: bool, now: Instant) -> Vec<PlaybackAction<A>> {
        if self.muted == muted {
            return Vec::new();
        }
        self.muted = muted;
        if !muted || !self.is_on() {
            return Vec::new();
        }
        let reply_done = self.reply_done;
        let actions = self.silence(Some(now));
        self.reply_done = reply_done;
        self.state = if reply_done {
            ConversationState::Listening
        } else {
            ConversationState::Thinking
        };
        actions
    }

    /// A prompt was sent; the reply that follows replaces whatever is being said.
    pub fn prompt_sent(&mut self) -> Vec<PlaybackAction<A>> {
        if !self.is_on() {
            return Vec::new();
        }
        let actions = self.silence(None);
        self.approvals.clear();
        self.reply_done = false;
        self.interrupted = false;
        self.state = ConversationState::Thinking;
        actions
    }

    /// Another reply of the same turn started streaming, such as the agent's follow-up after a
    /// tool call. It is spoken unless the user interrupted this turn.
    pub fn reply_started(&mut self) {
        if !self.is_on() || self.interrupted || !self.reply_done {
            return;
        }
        self.reply_done = false;
        if !self.is_speaking() {
            self.state = ConversationState::Thinking;
        }
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

    /// Speaks `text` after anything already queued, such as a tool-result summary. Nothing is
    /// said once the user has interrupted the turn.
    pub fn speak(&mut self, text: String) -> Vec<PlaybackAction<A>> {
        if !self.is_on() || self.interrupted {
            return Vec::new();
        }
        self.enqueue(text);
        self.pump()
    }

    /// The agent waits on approval `id` to run `command`; the prompt is spoken after the reply so
    /// far. Spoken answers count only once the prompt has played.
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
        if self.muted {
            return Vec::new();
        }
        for sentence in self.chunker.finish() {
            self.enqueue(sentence);
        }
        self.approval_prompt_seq = Some(self.next_seq);
        self.enqueue(prompt);
        let actions = self.pump();
        self.state = if self.is_speaking() {
            self.state
        } else {
            ConversationState::Listening
        };
        actions
    }

    /// The approval was answered on screen or went away.
    pub fn approval_resolved(&mut self, id: &Id) {
        self.approvals.resolved(id);
    }

    /// Closes the spoken answer window once it has passed, returning the speech that says the
    /// approval moved to the screen.
    pub fn check_approval_timeout(&mut self, now: Instant) -> Vec<PlaybackAction<A>> {
        if !self.is_on() || !self.approvals.check_timeout(now) {
            return Vec::new();
        }
        self.enqueue("I'll wait for you to approve it on screen.".to_owned());
        self.pump()
    }

    fn enqueue(&mut self, text: String) {
        if self.muted {
            return;
        }
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

    /// Records that sentence `seq` is done with (played or skipped) at `now`.
    fn sentence_done(&mut self, seq: u64, now: Instant) {
        if self.approval_prompt_seq == Some(seq) {
            self.approval_prompt_seq = None;
            self.approvals.prompt_played(now);
        }
    }

    /// Reports a synthesis result at `now`; failed sentences are skipped.
    pub fn synthesized(
        &mut self,
        reply: u64,
        seq: u64,
        audio: Option<A>,
        now: Instant,
    ) -> Vec<PlaybackAction<A>> {
        if reply != self.reply || self.synthesizing != Some(seq) {
            return Vec::new();
        }
        self.synthesizing = None;
        let mut actions = Vec::new();
        match audio {
            Some(audio) if self.playing.is_none() => {
                self.playing = Some(seq);
                self.playing_since = Some(now);
                self.state = ConversationState::Speaking;
                actions.push(PlaybackAction::Play { reply, seq, audio });
            }
            Some(audio) => self.ready.push_back((seq, audio)),
            None => self.sentence_done(seq, now),
        }
        actions.extend(self.pump());
        self.settle();
        actions
    }

    /// Reports that sentence `seq` finished playing at `now`.
    pub fn playback_finished(
        &mut self,
        reply: u64,
        seq: u64,
        now: Instant,
    ) -> Vec<PlaybackAction<A>> {
        if reply != self.reply || self.playing != Some(seq) {
            return Vec::new();
        }
        self.playing = None;
        self.sentence_done(seq, now);
        let mut actions = Vec::new();
        if let Some((seq, audio)) = self.ready.pop_front() {
            self.playing = Some(seq);
            actions.push(PlaybackAction::Play { reply, seq, audio });
        } else {
            self.playing_since = None;
            self.played_until = Some(now);
        }
        actions.extend(self.pump());
        if self.playing.is_none() && !self.reply_done {
            self.state = ConversationState::Thinking;
        }
        self.settle();
        actions
    }

    /// The VAD heard the start of speech at `now`.
    pub fn speech_started(&mut self, now: Instant) {
        self.talking = true;
        self.utterance = Some(Utterance {
            started_at: now,
            overlapped: self.is_audible(now),
            barged_in: false,
        });
    }

    /// The VAD heard the end of speech.
    pub fn speech_ended(&mut self) {
        self.talking = false;
    }

    /// Stops the speech once the user has talked over it long enough (see [`BARGE_IN_AFTER`]).
    /// The owner calls it every [`BARGE_IN_POLL`] while the user keeps talking.
    pub fn check_barge_in(&mut self, now: Instant) -> Vec<PlaybackAction<A>> {
        if !self.talking || !self.is_speaking() {
            return Vec::new();
        }
        let Some(utterance) = self.utterance else {
            return Vec::new();
        };
        let overlap_start = match self.playing_since {
            Some(since) => since.max(utterance.started_at),
            None => utterance.started_at,
        };
        if now.saturating_duration_since(overlap_start) < self.barge_in_after() {
            return Vec::new();
        }
        let reply = self.reply;
        let mut actions = self.silence(Some(now));
        actions.push(PlaybackAction::Interrupted { reply });
        if let Some(utterance) = &mut self.utterance {
            utterance.barged_in = true;
        }
        self.reply_done = true;
        self.interrupted = true;
        self.state = ConversationState::Listening;
        actions
    }

    /// Interprets a final utterance heard by `now`: likely echo is dropped, "stop" ends the speech
    /// while the assistant is speaking or thinking, and an approval phrase answers the pending
    /// approval; anything else goes to the input as usual.
    pub fn on_final(
        &mut self,
        text: &str,
        now: Instant,
    ) -> (FinalOutcome<Id>, Vec<PlaybackAction<A>>) {
        let utterance = self.utterance.take();
        if !self.is_on() {
            return (FinalOutcome::NotConsumed, Vec::new());
        }
        let barged_in = utterance.is_some_and(|utterance| utterance.barged_in);
        let overlapped = utterance.is_some_and(|utterance| utterance.overlapped);
        let assistant_busy = matches!(
            self.state,
            ConversationState::Speaking | ConversationState::Thinking
        );
        // "Stop" is short enough never to barge in on loudspeakers, and the assistant doesn't say
        // it on its own, so it is honoured even when it overlapped the speech.
        if (assistant_busy || barged_in || overlapped) && is_stop_phrase(text) {
            let actions = self.silence(Some(now));
            self.reply_done = true;
            self.interrupted = true;
            self.state = ConversationState::Listening;
            return (FinalOutcome::Stopped, actions);
        }
        if overlapped && !barged_in && !self.headphones {
            return (FinalOutcome::Echo, Vec::new());
        }
        let started_at = utterance.map(|utterance| utterance.started_at);
        match self.approvals.on_utterance(text, started_at, now) {
            ApprovalVerdict::NotAnAnswer => (FinalOutcome::NotConsumed, Vec::new()),
            ApprovalVerdict::Ignored => (FinalOutcome::Ignored, Vec::new()),
            ApprovalVerdict::Answered(id, reply) => (FinalOutcome::Answered(id, reply), Vec::new()),
        }
    }
}

#[cfg(test)]
#[path = "playback_tests.rs"]
mod tests;
