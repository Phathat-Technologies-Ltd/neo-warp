//! The conversation-mode state machine, free of I/O so it can be tested: it turns a streamed
//! reply into a queue of sentences that are synthesized one ahead of playback, and stops the
//! speech when the user talks over it or answers an approval.

use std::collections::VecDeque;
use std::time::Duration;

use instant::Instant;

use super::approval::{ApprovalFlow, ApprovalReply, ApprovalVerdict, ECHO_TAIL, is_stop_phrase};
use super::chunker::{ChunkerOptions, SentenceChunker};
use super::summary::grouped_approval_prompt;

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

/// The same on loudspeakers with acoustic echo cancellation, which removes the assistant from the
/// microphone signal. Loudspeakers without it are half-duplex: speech never barges in, and only a
/// stop phrase interrupts the assistant.
pub const BARGE_IN_AFTER_WITH_ECHO_CANCELLATION: Duration = Duration::from_millis(200);

/// How long after the assistant last spoke a transcript that repeats its words is dropped as echo.
pub const RECENT_SPEECH_WINDOW: Duration = Duration::from_secs(10);

/// How many of the assistant's latest sentences a transcript is compared with.
const RECENT_SENTENCES: usize = 8;

/// The share of a transcript's words that must appear in the assistant's recent speech for it to
/// be treated as echo.
const ECHO_WORD_OVERLAP: f32 = 0.8;

/// How long an approval request waits before it is announced, so one approved on screen right
/// away is never spoken and requests that come in together share one prompt.
pub const APPROVAL_DEBOUNCE: Duration = Duration::from_secs(1);

/// Asked again when only part of a spoken approval answer was heard.
const APPROVAL_REASK: &str = "Sorry, say yes, or no.";

/// The least time between two approval prompts of a turn while the earlier one is unresolved.
pub const APPROVAL_PROMPT_GAP: Duration = Duration::from_secs(8);

/// Speech gain while a candidate barge-in waits for its speaker check.
pub const BARGE_IN_DUCK_GAIN: f32 = 0.3;
/// Speech a candidate barge-in needs before its speaker check runs; shorter than the early check
/// for partials, since the reply stays ducked until the verdict.
pub const BARGE_IN_CHECK_AFTER: Duration = Duration::from_millis(700);
/// Later speaker checks of a ducked barge-in, on more audio: the first one hears the user mixed with
/// the reply's echo and often scores low.
pub const BARGE_IN_RECHECKS: [Duration; 2] =
    [Duration::from_millis(1200), Duration::from_millis(1800)];
/// How long a ducked reply waits for a sign of the user before resuming at full volume.
pub const BARGE_IN_VERDICT_TIMEOUT: Duration = Duration::from_millis(2500);

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
    /// Lower the speech volume to `gain` while a barge-in is checked.
    Duck { gain: f32 },
    /// Return the speech to full volume.
    Restore,
}

/// A speaker check's verdict on speech over the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BargeInSpeaker {
    Match,
    NoMatch,
    /// Too little speech to judge, or the check couldn't run.
    Unavailable,
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
    /// A ducked barge-in on this utterance resumed the reply; it never ducks again.
    barge_in_dismissed: bool,
}

/// An approval request that has not been announced yet.
#[derive(Debug, Clone)]
struct WaitingApproval<Id> {
    id: Id,
    subject: String,
    needs_screen: bool,
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
    /// Approval requests not announced yet, oldest first.
    waiting_approvals: Vec<WaitingApproval<Id>>,
    /// When the waiting approvals may be announced, counted from the first of them.
    approval_debounce_until: Option<Instant>,
    /// Approvals of this turn that were announced or resolved; they are never announced again.
    handled_approvals: Vec<Id>,
    /// The unresolved approvals the latest approval prompt covers.
    prompt_approvals: Vec<Id>,
    /// When the latest approval prompt was issued, while any approval it covers is unresolved.
    last_approval_prompt_at: Option<Instant>,
    /// Conversation mode stays on but nothing is read aloud.
    muted: bool,
    /// The microphone signal has the assistant's voice removed by acoustic echo cancellation.
    echo_cancelled: bool,
    /// The words of the assistant's latest sentences, oldest first.
    recent_speech: VecDeque<Vec<String>>,
    /// Barge-in waits for a speaker check against the enrolled voiceprint.
    verify_barge_in: bool,
    /// When the speech was ducked for a barge-in awaiting its speaker check.
    ducked_at: Option<Instant>,
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
            waiting_approvals: Vec::new(),
            approval_debounce_until: None,
            handled_approvals: Vec::new(),
            prompt_approvals: Vec::new(),
            last_approval_prompt_at: None,
            muted: false,
            echo_cancelled: false,
            recent_speech: VecDeque::new(),
            verify_barge_in: false,
            ducked_at: None,
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
            echo_cancelled: self.echo_cancelled,
            played_until: self.played_until,
            talking: self.talking,
            utterance: self.utterance,
            recent_speech: std::mem::take(&mut self.recent_speech),
            ..Self::default()
        };
    }

    /// Turns conversation mode off at `now`, silencing the speech. What the microphone may still
    /// hear of it (the utterance under way, the echo tail, the recent sentences) stays known, so
    /// that echo is dropped even after the mode is off or restarted.
    pub fn stop(&mut self, now: Instant) -> Vec<PlaybackAction<A>> {
        let actions = self.silence(Some(now));
        *self = Self {
            reply: self.reply + 1,
            played_until: self.played_until,
            talking: self.talking,
            utterance: self.utterance,
            headphones: self.headphones,
            recent_speech: std::mem::take(&mut self.recent_speech),
            ..Self::default()
        };
        actions
    }

    /// Whether the output is headphones, which the microphone doesn't hear, so barge-in can be
    /// quick and speech over the assistant is trusted.
    pub fn set_headphones(&mut self, headphones: bool) {
        self.headphones = headphones;
    }

    /// Whether the microphone signal has acoustic echo cancellation, so speech over the assistant
    /// on loudspeakers can be trusted once it lasts [`BARGE_IN_AFTER_WITH_ECHO_CANCELLATION`].
    pub fn set_echo_cancelled(&mut self, echo_cancelled: bool) {
        self.echo_cancelled = echo_cancelled;
    }

    /// Whether the microphone hears the assistant unfiltered, so nothing heard over it is used.
    fn is_half_duplex(&self) -> bool {
        !self.headphones && !self.echo_cancelled
    }

    /// Whether transcripts heard at `now` are dropped because the microphone may be hearing the
    /// assistant; the owner keeps partials off screen and holds auto-send meanwhile.
    pub fn is_echo_window(&self, now: Instant) -> bool {
        self.is_half_duplex() && self.is_audible(now)
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

    /// Whether barge-in waits for a speaker check: speech over the reply ducks it, and only the
    /// enrolled user's voice stops it.
    pub fn set_verify_barge_in(&mut self, verify: bool) {
        self.verify_barge_in = verify;
    }

    /// Whether the speech is ducked waiting for a barge-in speaker check.
    pub fn is_barge_in_pending(&self) -> bool {
        self.ducked_at.is_some()
    }

    fn barge_in_after(&self) -> Option<Duration> {
        if self.headphones {
            Some(BARGE_IN_AFTER)
        } else if self.echo_cancelled {
            Some(BARGE_IN_AFTER_WITH_ECHO_CANCELLATION)
        } else {
            None
        }
    }

    /// Whether `text` mostly repeats what the assistant said recently.
    fn repeats_recent_speech(&self, text: &str, now: Instant) -> bool {
        let spoke_recently = self.playing.is_some()
            || self
                .played_until
                .is_some_and(|until| now < until + RECENT_SPEECH_WINDOW);
        if !spoke_recently {
            return false;
        }
        let heard = speech_words(text);
        if heard.len() < 2 {
            return false;
        }
        let matched = heard
            .iter()
            .filter(|word| {
                self.recent_speech
                    .iter()
                    .any(|sentence| sentence.contains(word))
            })
            .count();
        matched as f32 >= heard.len() as f32 * ECHO_WORD_OVERLAP
    }

    /// Records that the assistant is now audible, so speech already under way counts as
    /// overlapping it.
    fn became_audible(&mut self) {
        if self.talking
            && let Some(utterance) = &mut self.utterance
        {
            utterance.overlapped = true;
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
        let restore = self.ducked_at.take().is_some();
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
        let mut actions = Vec::new();
        if was_playing {
            actions.push(PlaybackAction::StopPlayback);
        }
        if restore {
            actions.push(PlaybackAction::Restore);
        }
        actions
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
        self.waiting_approvals.clear();
        self.approval_debounce_until = None;
        self.handled_approvals.clear();
        self.prompt_approvals.clear();
        self.last_approval_prompt_at = None;
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

    /// The agent waits at `now` on approval `id` to do `subject` (see
    /// [`super::approval::approval_prompt`]). The prompt is spoken after [`APPROVAL_DEBOUNCE`] (see
    /// [`Self::approval_due_at`]), after the reply so far, and never for an approval of this turn
    /// that was already announced or resolved.
    pub fn approval_requested(&mut self, id: Id, subject: &str, needs_screen: bool, now: Instant) {
        if !self.is_on()
            || self.handled_approvals.contains(&id)
            || self
                .waiting_approvals
                .iter()
                .any(|waiting| waiting.id == id)
        {
            return;
        }
        self.waiting_approvals.push(WaitingApproval {
            id,
            subject: subject.to_owned(),
            needs_screen,
        });
        self.approval_debounce_until
            .get_or_insert(now + APPROVAL_DEBOUNCE);
    }

    /// When the waiting approval requests are due to be announced with
    /// [`Self::check_approval_due`].
    pub fn approval_due_at(&self) -> Option<Instant> {
        if self.waiting_approvals.is_empty() {
            return None;
        }
        let gap_until = self
            .last_approval_prompt_at
            .map(|at| at + APPROVAL_PROMPT_GAP);
        self.approval_debounce_until.max(gap_until)
    }

    /// Announces the waiting approval requests once they are due at `now`: one prompt for all of
    /// them. Spoken answers count only for a single request, once its prompt has played.
    pub fn check_approval_due(&mut self, now: Instant) -> Vec<PlaybackAction<A>> {
        if !self.is_on() || !self.approval_due_at().is_some_and(|due| now >= due) {
            return Vec::new();
        }
        self.approval_debounce_until = None;
        let waiting = std::mem::take(&mut self.waiting_approvals);
        let count = waiting.len();
        let ids = waiting
            .iter()
            .map(|waiting| waiting.id.clone())
            .collect::<Vec<_>>();
        self.handled_approvals.extend(ids.iter().cloned());
        self.prompt_approvals = ids;
        self.last_approval_prompt_at = Some(now);
        let Some(first) = waiting.into_iter().next() else {
            return Vec::new();
        };
        let prompt = if count == 1 {
            self.approvals
                .request(first.id, &first.subject, first.needs_screen)
        } else {
            self.approvals.request(first.id, &first.subject, true);
            grouped_approval_prompt(count)
        };
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

    /// The approval was answered on screen or by voice, ran, or went away at `now`. Its prompt is
    /// dropped if queued and stopped if playing, and its spoken answer window closes.
    pub fn approval_resolved(&mut self, id: &Id, now: Instant) -> Vec<PlaybackAction<A>> {
        self.approvals.resolved(id);
        self.waiting_approvals.retain(|waiting| &waiting.id != id);
        if self.waiting_approvals.is_empty() {
            self.approval_debounce_until = None;
        }
        if !self.handled_approvals.contains(id) {
            self.handled_approvals.push(id.clone());
        }
        if !self.prompt_approvals.contains(id) {
            return Vec::new();
        }
        self.prompt_approvals.retain(|prompted| prompted != id);
        if self.prompt_approvals.is_empty() {
            self.last_approval_prompt_at = None;
        }
        match self.approval_prompt_seq.take() {
            Some(seq) => self.drop_sentence(seq, now),
            None => Vec::new(),
        }
    }

    /// Drops sentence `seq` at `now` wherever it is in the pipeline, stopping it if it plays and
    /// moving on to the next one.
    fn drop_sentence(&mut self, seq: u64, now: Instant) -> Vec<PlaybackAction<A>> {
        self.queued.retain(|(queued, _)| *queued != seq);
        self.ready.retain(|(ready, _)| *ready != seq);
        if self.synthesizing == Some(seq) {
            self.synthesizing = None;
        }
        let mut actions = Vec::new();
        if self.playing == Some(seq) {
            self.playing = None;
            actions.push(PlaybackAction::StopPlayback);
            if let Some((next, audio)) = self.ready.pop_front() {
                self.playing = Some(next);
                actions.push(PlaybackAction::Play {
                    reply: self.reply,
                    seq: next,
                    audio,
                });
            } else {
                self.playing_since = None;
                self.played_until = Some(now);
            }
        }
        actions.extend(self.pump());
        if self.is_on() && !self.is_speaking() {
            self.state = if self.reply_done {
                ConversationState::Listening
            } else {
                ConversationState::Thinking
            };
        }
        actions
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
        let text = super::spoken::plain_speech(&text);
        if text.is_empty() {
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
        if self.recent_speech.len() == RECENT_SENTENCES {
            self.recent_speech.pop_front();
        }
        self.recent_speech.push_back(speech_words(&text));
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
                self.became_audible();
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
            self.became_audible();
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
            barge_in_dismissed: false,
        });
    }

    /// The VAD heard the end of speech.
    pub fn speech_ended(&mut self) {
        self.talking = false;
    }

    /// Stops the speech once the user has talked over it long enough (see [`BARGE_IN_AFTER`]).
    /// With [`Self::set_verify_barge_in`] on, it ducks the speech instead until the user is
    /// recognised ([`Self::barge_in_verdict`], [`Self::barge_in_transcript`]) or
    /// [`BARGE_IN_VERDICT_TIMEOUT`] passes. The owner calls it every [`BARGE_IN_POLL`] while the
    /// user keeps talking, and once the timeout has passed.
    pub fn check_barge_in(&mut self, now: Instant) -> Vec<PlaybackAction<A>> {
        if let Some(ducked_at) = self.ducked_at {
            if now.saturating_duration_since(ducked_at) >= BARGE_IN_VERDICT_TIMEOUT {
                log::info!("Barge-in decision: resume (timeout, no sign of the user)");
                return self.dismiss_barge_in();
            }
            return Vec::new();
        }
        if !self.talking || !self.is_speaking() {
            return Vec::new();
        }
        let Some(utterance) = self.utterance else {
            return Vec::new();
        };
        if utterance.barge_in_dismissed {
            return Vec::new();
        }
        let Some(barge_in_after) = self.barge_in_after() else {
            return Vec::new();
        };
        let overlap_start = match self.playing_since {
            Some(since) => since.max(utterance.started_at),
            None => utterance.started_at,
        };
        if now.saturating_duration_since(overlap_start) < barge_in_after {
            return Vec::new();
        }
        if self.verify_barge_in {
            log::info!("Possible barge-in: ducking the reply until the speaker is checked");
            self.ducked_at = Some(now);
            return vec![PlaybackAction::Duck {
                gain: BARGE_IN_DUCK_GAIN,
            }];
        }
        self.interrupt(now)
    }

    /// Feeds a speaker check of the speech over a ducked reply: a match stops the reply. A
    /// mismatch or an unavailable check keeps it ducked, since the user's voice mixed with the
    /// reply's echo often scores low; the transcript or the timeout decides instead.
    pub fn barge_in_verdict(
        &mut self,
        verdict: BargeInSpeaker,
        now: Instant,
    ) -> Vec<PlaybackAction<A>> {
        if self.ducked_at.is_none() {
            return Vec::new();
        }
        match verdict {
            BargeInSpeaker::Match => {
                log::info!("Barge-in decision: stop the reply (speaker check matched)");
                self.interrupt(now)
            }
            BargeInSpeaker::NoMatch => {
                log::info!("Barge-in: speaker check did not match; staying ducked for more signal");
                Vec::new()
            }
            BargeInSpeaker::Unavailable => {
                log::info!("Barge-in: speaker check inconclusive; staying ducked for more signal");
                Vec::new()
            }
        }
    }

    /// Feeds a transcript of the speech over a ducked reply: words the reply just said are its
    /// echo and resume it; two or more other words (or a stop phrase) are the user and stop it.
    pub fn barge_in_transcript(&mut self, text: &str, now: Instant) -> Vec<PlaybackAction<A>> {
        if self.ducked_at.is_none() {
            return Vec::new();
        }
        if self.repeats_recent_speech(text, now) {
            log::info!("Barge-in decision: resume (transcript repeats the reply)");
            return self.dismiss_barge_in();
        }
        if speech_words(text).len() >= 2 || is_stop_phrase(text) {
            log::info!("Barge-in decision: stop the reply (transcript differs from the reply)");
            return self.interrupt(now);
        }
        Vec::new()
    }

    /// Resumes a ducked reply and keeps the speech over it from ducking it again.
    fn dismiss_barge_in(&mut self) -> Vec<PlaybackAction<A>> {
        self.ducked_at = None;
        if let Some(utterance) = &mut self.utterance {
            utterance.barge_in_dismissed = true;
        }
        vec![PlaybackAction::Restore]
    }

    fn interrupt(&mut self, now: Instant) -> Vec<PlaybackAction<A>> {
        let reply = self.reply;
        let prompt_cut_off = self.approval_prompt_seq.is_some();
        let mut actions = self.silence(Some(now));
        // Talking over the approval prompt is often the answer itself; never leave it unanswerable.
        if prompt_cut_off {
            let talking_from = self.utterance.map_or(now, |utterance| utterance.started_at);
            self.approvals.prompt_interrupted(talking_from, now);
        }
        actions.push(PlaybackAction::Interrupted { reply });
        if let Some(utterance) = &mut self.utterance {
            utterance.barged_in = true;
        }
        self.reply_done = true;
        self.interrupted = true;
        self.state = ConversationState::Listening;
        actions
    }

    /// Repeats the question of the pending approval, whose answer window reopens once it played.
    fn ask_approval_again(&mut self) -> Vec<PlaybackAction<A>> {
        if self.muted || self.approval_prompt_seq.is_some() {
            return Vec::new();
        }
        self.approval_prompt_seq = Some(self.next_seq);
        self.enqueue(APPROVAL_REASK.to_owned());
        self.pump()
    }

    /// Interprets a final utterance heard by `now`: likely echo is dropped, "stop" ends the speech
    /// while the assistant is speaking or thinking, and an approval phrase answers the pending
    /// approval; anything else goes to the input as usual.
    pub fn on_final(
        &mut self,
        text: &str,
        now: Instant,
    ) -> (FinalOutcome<Id>, Vec<PlaybackAction<A>>) {
        self.on_final_from(text, false, now)
    }

    /// Like [`Self::on_final`], for a final whose speaker check matched the enrolled user when
    /// `speaker_verified`. Such a final is the user's own voice, so overlapping the assistant's
    /// speech doesn't make it echo; long utterances overlap a streaming reply or its echo tail
    /// often.
    pub fn on_final_from(
        &mut self,
        text: &str,
        speaker_verified: bool,
        now: Instant,
    ) -> (FinalOutcome<Id>, Vec<PlaybackAction<A>>) {
        // A stop phrase is handled below without counting as an interruption.
        let mut actions = if is_stop_phrase(text) {
            Vec::new()
        } else {
            self.barge_in_transcript(text, now)
        };
        let (outcome, more) = self.interpret_final(text, speaker_verified, now);
        actions.extend(more);
        (outcome, actions)
    }

    fn interpret_final(
        &mut self,
        text: &str,
        speaker_verified: bool,
        now: Instant,
    ) -> (FinalOutcome<Id>, Vec<PlaybackAction<A>>) {
        let utterance = self.utterance.take();
        if !self.is_on() {
            let overlapped =
                utterance.is_some_and(|utterance| utterance.overlapped) || self.is_audible(now);
            // With echo cancellation, overlapping the assistant isn't evidence of echo by itself;
            // only words that repeat what it said are.
            let echo = (overlapped && self.is_half_duplex() && !speaker_verified)
                || self.repeats_recent_speech(text, now);
            let outcome = if echo {
                FinalOutcome::Echo
            } else {
                FinalOutcome::NotConsumed
            };
            return (outcome, Vec::new());
        }
        let barged_in = utterance.is_some_and(|utterance| utterance.barged_in);
        // A final can arrive without a speech start (the VAD missed it), and a long segment can
        // end inside the echo tail, so being audible now counts as overlap too.
        let overlapped =
            utterance.is_some_and(|utterance| utterance.overlapped) || self.is_audible(now);
        let assistant_busy = matches!(
            self.state,
            ConversationState::Speaking | ConversationState::Thinking
        );
        let started_at = utterance.map(|utterance| utterance.started_at);
        // Once the approval prompt has played, "stop" answers it rather than stopping speech.
        let answers_approval =
            !self.is_speaking() && self.approvals.accepts_answer(started_at, now);
        // "Stop" is short enough never to barge in on loudspeakers, and the assistant doesn't say
        // it on its own, so it is honoured even when it overlapped the speech.
        if (assistant_busy || barged_in || overlapped) && !answers_approval && is_stop_phrase(text)
        {
            let actions = self.silence(Some(now));
            self.reply_done = true;
            self.interrupted = true;
            self.state = ConversationState::Listening;
            return (FinalOutcome::Stopped, actions);
        }
        if overlapped && self.is_half_duplex() && !speaker_verified {
            return (FinalOutcome::Echo, Vec::new());
        }
        // Approval answers come first: they reuse the prompt's own words ("say yes, or no").
        match self.approvals.on_utterance(text, started_at, now) {
            ApprovalVerdict::NotAnAnswer => {}
            ApprovalVerdict::Ignored | ApprovalVerdict::Held => {
                return (FinalOutcome::Ignored, Vec::new());
            }
            ApprovalVerdict::Unclear => {
                log::info!("Approval answer unclear; asking again");
                return (FinalOutcome::Ignored, self.ask_approval_again());
            }
            ApprovalVerdict::Answered(id, reply) => {
                return (FinalOutcome::Answered(id, reply), Vec::new());
            }
        }
        if self.repeats_recent_speech(text, now) {
            return (FinalOutcome::Echo, Vec::new());
        }
        (FinalOutcome::NotConsumed, Vec::new())
    }
}

/// The lowercase words of `text`, without punctuation.
fn speech_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

#[cfg(test)]
#[path = "playback_tests.rs"]
mod tests;
