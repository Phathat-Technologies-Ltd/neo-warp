//! Voice conversation mode: hands-free input plus agent replies spoken aloud on this computer.
//! Synthesis runs in the client, one sentence ahead of playback; the embedded server only does
//! speech-to-text.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use futures::channel::{mpsc, oneshot};
use instant::Instant;
use local_transcription::Audio;
use local_transcription::speech::approval::ApprovalReply;
use local_transcription::speech::chunker::ChunkerOptions;
use local_transcription::speech::playback::{
    BARGE_IN_POLL, ConversationController, ConversationState, FinalOutcome, PlaybackAction,
    is_headphone_device_name,
};
use local_transcription::speech::{KokoroSpeaker, Speaker, TinyVocaSpeaker, VoiceId};
use voice_input::SpeechOutput;
use warpui::r#async::Timer;
use warpui::{AppContext, Entity, EntityId, ModelContext, ModelHandle, SingletonEntity, WindowId};

use super::conversation_text::{
    ActionAnnouncement, ProgressAnnouncer, action_announcement, reply_actions,
    reply_has_spoken_summary, spoken_action_prompt, spoken_action_result, spoken_reply_text,
};
use super::hands_free::{HandsFreeEvent, HandsFreeSession, toggle_hands_free};
use super::local_model::{
    SpeechOutputEngine, SpeechOutputModel, ensure_speech_output_ready, is_conversation_enabled,
    speech_output_store,
};
use crate::ai::agent::api::VOICE_CONVERSATION_ACTIVE;
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent::{AIAgentActionId, AIAgentActionResultType, CancellationReason};
use crate::ai::blocklist::{
    BlocklistAIActionModel, BlocklistAIHistoryEvent, BlocklistAIHistoryModel,
};
use crate::settings::AISettings;
use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;

pub enum ConversationEvent {
    StateChanged,
}

/// Whether moving the hands-free target to `target` moves which pane's replies are read.
pub fn surface_changes(current: Option<EntityId>, target: Option<EntityId>) -> bool {
    target.is_some() && current != target
}

/// The action model and conversation of the approval being asked about.
struct ApprovalTarget {
    action_model: ModelHandle<BlocklistAIActionModel>,
    conversation_id: AIConversationId,
}

/// The reply being spoken: which exchange, and how much of its text was fed to the chunker.
struct SpokenReply {
    exchange_id: crate::ai::agent::AIAgentExchangeId,
    fed: String,
    /// Whether the reply has a `<spoken>` summary, which covers its tool calls and results.
    has_spoken: bool,
    /// How many of the reply's tool calls were already seen.
    announced_actions: usize,
    progress: ProgressAnnouncer,
}

pub struct ConversationSession {
    controller: ConversationController<Arc<Audio>, AIAgentActionId>,
    speaker: Option<Arc<dyn Speaker>>,
    voice: VoiceId,
    rate: f32,
    output: Option<SpeechOutput>,
    window_id: Option<WindowId>,
    /// The terminal surface whose agent replies are spoken.
    surface: Option<EntityId>,
    reply: Option<SpokenReply>,
    approval: Option<ApprovalTarget>,
    /// The end of the spoken answer window a timer is already waiting for.
    approval_timeout_at: Option<Instant>,
    /// When the progress line timer already waiting will fire.
    progress_timer_at: Option<Instant>,
    finished_tx: mpsc::UnboundedSender<(u64, u64)>,
}

impl ConversationSession {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let (finished_tx, finished_rx) = mpsc::unbounded();
        ctx.spawn_stream_local(
            finished_rx,
            |me, (reply, seq), ctx| {
                let actions = me.controller.playback_finished(reply, seq, Instant::now());
                me.apply(actions, ctx);
            },
            |_, _| {},
        );
        ctx.subscribe_to_model(&HandsFreeSession::handle(ctx), |me, session, event, ctx| {
            if !matches!(event, HandsFreeEvent::StateChanged) || !me.is_active() {
                return;
            }
            let session = session.as_ref(ctx);
            let target = session.target_surface();
            if !session.is_active() {
                me.stop(ctx);
                return;
            } else if !session.state().is_mic_on() {
                // Paused because Warp isn't the active app: nothing is spoken over other apps.
                me.stop_speaking(ctx);
            }
            me.follow_target(target, ctx);
        });
        ctx.subscribe_to_model(
            &BlocklistAIHistoryModel::handle(ctx),
            |me, _, event, ctx| {
                me.handle_history_event(event, ctx);
            },
        );
        ctx.subscribe_to_model(&AISettings::handle(ctx), |_, _, _, ctx| {
            let engine = SpeechOutputEngine::from_setting(
                &AISettings::as_ref(ctx).conversation_speech_engine,
            );
            SpeechOutputModel::handle(ctx).update(ctx, |model, ctx| model.set_engine(engine, ctx));
        });
        Self {
            controller: ConversationController::default(),
            speaker: None,
            voice: VoiceId::new(""),
            rate: 1.0,
            output: None,
            window_id: None,
            surface: None,
            reply: None,
            approval: None,
            approval_timeout_at: None,
            progress_timer_at: None,
            finished_tx,
        }
    }

    pub fn is_active(&self) -> bool {
        self.controller.state() != ConversationState::Off
    }

    pub fn state(&self) -> ConversationState {
        self.controller.state()
    }

    /// Whether a reply is being read aloud, so Esc and the Stop button stop it.
    pub fn is_speaking(&self) -> bool {
        self.is_active() && self.controller.is_speaking()
    }

    /// Whether the microphone may be hearing the reply right now, so partial transcripts stay off
    /// screen and auto-send waits.
    pub fn is_echo_window(&self) -> bool {
        self.controller.is_echo_window(Instant::now())
    }

    pub fn is_muted(&self) -> bool {
        self.controller.is_muted()
    }

    /// Stops reading the current reply aloud. Returns whether anything was being read.
    pub fn stop_speaking(&mut self, ctx: &mut ModelContext<Self>) -> bool {
        if !self.is_speaking() {
            return false;
        }
        let actions = self.controller.stop_speaking(Instant::now());
        self.apply(actions, ctx);
        ctx.emit(ConversationEvent::StateChanged);
        ctx.notify();
        true
    }

    /// Mutes or unmutes the readout; conversation mode stays on either way.
    pub fn toggle_muted(&mut self, ctx: &mut ModelContext<Self>) {
        let muted = !self.controller.is_muted();
        let actions = self.controller.set_muted(muted, Instant::now());
        self.apply(actions, ctx);
        ctx.emit(ConversationEvent::StateChanged);
        ctx.notify();
    }

    /// Whether `window_id` is where conversation mode runs.
    pub fn is_in_window(&self, window_id: WindowId) -> bool {
        self.is_active() && self.window_id == Some(window_id)
    }

    fn start(&mut self, window_id: WindowId, ctx: &mut ModelContext<Self>) -> Result<(), String> {
        let settings = AISettings::as_ref(ctx);
        let engine = SpeechOutputEngine::from_setting(&settings.conversation_speech_engine);
        let store = speech_output_store(engine);
        let speaker: Arc<dyn Speaker> = match engine {
            SpeechOutputEngine::Kokoro => Arc::new(KokoroSpeaker::new(store)),
            SpeechOutputEngine::TinyVoca => Arc::new(TinyVocaSpeaker::new(store)),
        };
        let voices = speaker.voices();
        let chosen = settings.conversation_voice.as_str();
        self.voice = voices
            .iter()
            .find(|voice| voice.id.as_str() == chosen)
            .or_else(|| voices.first())
            .map(|voice| voice.id.clone())
            .ok_or_else(|| "The speech output model has no voices".to_owned())?;
        self.rate = settings.conversation_speech_rate();
        let options = ChunkerOptions {
            read_code_summaries: *settings.conversation_read_code_summaries,
        };
        // The microphone and the speaker must both be on the echo-cancelling unit, so hands-free
        // capture moves onto it as well.
        voice_input::set_voice_processing_wanted(*settings.conversation_echo_cancellation);
        let output = SpeechOutput::open().map_err(|error| {
            voice_input::set_voice_processing_wanted(false);
            format!("Couldn't open the speaker: {error:#}")
        })?;
        let headphones =
            output.reports_headphones() || is_headphone_device_name(output.device_name());
        self.output = Some(output);
        // Loading the model takes seconds, so do it now rather than on the first sentence.
        let warm = speaker.clone();
        let voice = self.voice.clone();
        std::thread::spawn(move || {
            if let Err(error) = warm.synthesize("Ready.", &voice, 1.0) {
                log::warn!("Couldn't load the speech output model: {error}");
            }
        });
        self.speaker = Some(speaker);
        self.window_id = Some(window_id);
        self.controller.start(options);
        self.controller.set_headphones(headphones);
        self.refresh_echo_cancellation();
        VOICE_CONVERSATION_ACTIVE.store(true, Ordering::Relaxed);
        ctx.emit(ConversationEvent::StateChanged);
        ctx.notify();
        Ok(())
    }

    pub fn stop(&mut self, ctx: &mut ModelContext<Self>) {
        if !self.is_active() {
            return;
        }
        let actions = self.controller.stop(Instant::now());
        self.apply(actions, ctx);
        VOICE_CONVERSATION_ACTIVE.store(false, Ordering::Relaxed);
        self.output = None;
        voice_input::set_voice_processing_wanted(false);
        self.speaker = None;
        self.surface = None;
        self.reply = None;
        self.approval = None;
        self.approval_timeout_at = None;
        ctx.emit(ConversationEvent::StateChanged);
        ctx.notify();
    }

    /// Reads replies only from the hands-free target's pane: when the target moves to another
    /// tab or pane, the reply being read stops and the new pane's replies are read instead.
    fn follow_target(&mut self, target: Option<EntityId>, ctx: &mut ModelContext<Self>) {
        if target.is_none() || !surface_changes(self.surface, target) {
            return;
        }
        self.stop_speaking(ctx);
        self.surface = target;
        self.reply = None;
        self.approval = None;
        ctx.emit(ConversationEvent::StateChanged);
        ctx.notify();
    }

    /// A prompt was sent from the input of `surface` in `window_id`; its reply is spoken.
    pub fn prompt_sent(
        &mut self,
        surface: EntityId,
        window_id: WindowId,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.is_in_window(window_id) {
            return;
        }
        self.surface = Some(surface);
        self.reply = None;
        self.approval = None;
        let actions = self.controller.prompt_sent();
        self.apply(actions, ctx);
    }

    fn handle_history_event(
        &mut self,
        event: &BlocklistAIHistoryEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        let (exchange_id, conversation_id, surface) = match event {
            BlocklistAIHistoryEvent::UpdatedStreamingExchange {
                exchange_id,
                conversation_id,
                terminal_surface_id,
                is_hidden: false,
            }
            | BlocklistAIHistoryEvent::AppendedExchange {
                exchange_id,
                conversation_id,
                terminal_surface_id,
                is_hidden: false,
                ..
            } => (*exchange_id, *conversation_id, *terminal_surface_id),
            _ => return,
        };
        if !self.is_active() || self.surface != Some(surface) {
            return;
        }
        let history = BlocklistAIHistoryModel::as_ref(ctx);
        let Some(exchange) = history
            .conversation(&conversation_id)
            .and_then(|conversation| conversation.exchange_with_id(exchange_id))
        else {
            return;
        };
        let finished = exchange.output_status.is_finished();
        let (text, has_spoken, announcements) = exchange
            .output_status
            .output()
            .map(|output| {
                let output = output.get();
                (
                    spoken_reply_text(&output, finished),
                    reply_has_spoken_summary(&output),
                    reply_actions(&output)
                        .map(action_announcement)
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap_or_default();
        let reply = match &mut self.reply {
            Some(reply) if reply.exchange_id == exchange_id => reply,
            _ => {
                self.controller.reply_started();
                self.reply.insert(SpokenReply {
                    exchange_id,
                    fed: String::new(),
                    has_spoken: false,
                    announced_actions: 0,
                    progress: ProgressAnnouncer::default(),
                })
            }
        };
        reply.has_spoken |= has_spoken;
        let common = common_prefix_len(&reply.fed, &text);
        let delta = text[common..].to_owned();
        reply.fed = text;
        let now = Instant::now();
        let mut immediate = Vec::new();
        for announcement in announcements.into_iter().skip(reply.announced_actions) {
            reply.announced_actions += 1;
            if reply.has_spoken || finished {
                continue;
            }
            match announcement {
                ActionAnnouncement::Progress(kind, count) => {
                    reply.progress.action_started(kind, count, now);
                }
                ActionAnnouncement::Immediate(line) => immediate.push(line),
                ActionAnnouncement::Silent => {}
            }
        }
        if reply.has_spoken || finished {
            reply.progress.cancel();
        }
        let mut actions = self.controller.reply_text(&delta);
        for line in immediate {
            actions.extend(self.controller.speak(line));
        }
        if finished {
            actions.extend(self.controller.reply_finished());
        }
        self.apply(actions, ctx);
        self.schedule_progress(ctx);
    }

    /// Starts a timer for when the reply's pending progress line is due.
    fn schedule_progress(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(due) = self
            .reply
            .as_ref()
            .and_then(|reply| reply.progress.due_at())
        else {
            return;
        };
        if self.progress_timer_at.is_some_and(|at| at <= due) {
            return;
        }
        self.progress_timer_at = Some(due);
        ctx.spawn(
            Timer::after(due.saturating_duration_since(Instant::now())),
            |me, _, ctx| {
                me.progress_timer_at = None;
                let line = me
                    .reply
                    .as_mut()
                    .and_then(|reply| reply.progress.take_due(Instant::now()));
                if let Some(line) = line {
                    let actions = me.controller.speak(line);
                    me.apply(actions, ctx);
                }
                me.schedule_progress(ctx);
            },
        );
    }

    /// The agent waits on the user to approve an action in `surface`.
    pub fn approval_requested(
        &mut self,
        surface: EntityId,
        action_model: ModelHandle<BlocklistAIActionModel>,
        conversation_id: AIConversationId,
        action_id: AIAgentActionId,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.is_active() || self.surface != Some(surface) {
            return;
        }
        let Some((command, needs_screen)) = action_model
            .as_ref(ctx)
            .get_pending_action_by_id(&action_id)
            .map(|action| spoken_action_prompt(&action.action))
        else {
            return;
        };
        if let Some(reply) = &mut self.reply {
            reply.progress.cancel();
        }
        self.approval = Some(ApprovalTarget {
            action_model,
            conversation_id,
        });
        let actions = self
            .controller
            .approval_requested(action_id, &command, needs_screen);
        self.apply(actions, ctx);
    }

    /// An agent action of `surface` finished; a command's outcome gets a one-line note unless the
    /// reply's `<spoken>` summary covers the turn.
    pub fn action_finished(
        &mut self,
        surface: EntityId,
        result: &AIAgentActionResultType,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.is_active() || self.surface != Some(surface) {
            return;
        }
        if self.reply.as_ref().is_some_and(|reply| reply.has_spoken) {
            return;
        }
        let Some(text) = spoken_action_result(result) else {
            return;
        };
        let actions = self.controller.speak(text);
        self.apply(actions, ctx);
    }

    /// The action stopped waiting for approval (answered on screen, run, or cancelled).
    pub fn approval_resolved(&mut self, action_id: &AIAgentActionId) {
        self.controller.approval_resolved(action_id);
    }

    /// Lets conversation mode handle a final utterance before hands-free applies it. Returns
    /// whether it was consumed ("stop", or an approval answer) and must not reach the input.
    pub fn intercept_final(&mut self, text: &str, ctx: &mut ModelContext<Self>) -> bool {
        self.refresh_echo_cancellation();
        let (outcome, actions) = self.controller.on_final(text, Instant::now());
        self.apply(actions, ctx);
        match outcome {
            FinalOutcome::NotConsumed => false,
            FinalOutcome::Stopped | FinalOutcome::Ignored | FinalOutcome::Echo => true,
            FinalOutcome::Answered(action_id, reply) => {
                if let Some(target) = self.approval.take() {
                    let conversation_id = target.conversation_id;
                    target.action_model.update(ctx, |model, ctx| match reply {
                        ApprovalReply::Approve => {
                            model.execute_action(&action_id, conversation_id, ctx)
                        }
                        ApprovalReply::Reject => model.cancel_action_with_id(
                            conversation_id,
                            &action_id,
                            CancellationReason::ManuallyCancelled,
                            ctx,
                        ),
                    });
                }
                true
            }
        }
    }

    /// The VAD heard speech start; talking over the reply for long enough stops it.
    pub fn speech_started(&mut self, ctx: &mut ModelContext<Self>) {
        if !self.is_active() {
            return;
        }
        self.refresh_echo_cancellation();
        self.controller.speech_started(Instant::now());
        self.poll_barge_in(ctx);
    }

    /// Re-checks barge-in until the user stops talking or the speech stops.
    fn poll_barge_in(&mut self, ctx: &mut ModelContext<Self>) {
        ctx.spawn(Timer::after(BARGE_IN_POLL), |me, _, ctx| {
            if !me.is_active() || !me.controller.is_user_talking() {
                return;
            }
            let actions = me.controller.check_barge_in(Instant::now());
            let interrupted = !actions.is_empty();
            me.apply(actions, ctx);
            if !interrupted {
                me.poll_barge_in(ctx);
            }
        });
    }

    /// Starts a timer for the end of the spoken answer window once the approval prompt played.
    fn schedule_approval_timeout(&mut self, ctx: &mut ModelContext<Self>) {
        let until = self
            .controller
            .approvals()
            .pending()
            .filter(|pending| !pending.needs_screen)
            .and_then(|pending| pending.listening_until);
        let Some(until) = until else {
            return;
        };
        if self.approval_timeout_at == Some(until) {
            return;
        }
        self.approval_timeout_at = Some(until);
        ctx.spawn(
            Timer::after(until.saturating_duration_since(Instant::now())),
            |me, _, ctx| {
                let actions = me.controller.check_approval_timeout(Instant::now());
                me.apply(actions, ctx);
            },
        );
    }

    pub fn speech_ended(&mut self) {
        self.controller.speech_ended();
    }

    /// Trusts speech over the reply only while the microphone and speaker are actually on the
    /// echo-cancelling unit; it can fall back to the plain audio path at any time.
    fn refresh_echo_cancellation(&mut self) {
        self.controller
            .set_echo_cancelled(voice_input::is_echo_cancelling());
    }

    fn apply(&mut self, actions: Vec<PlaybackAction<Arc<Audio>>>, ctx: &mut ModelContext<Self>) {
        let before = self.controller.state();
        for action in actions {
            match action {
                PlaybackAction::Synthesize { reply, seq, text } => {
                    let Some(speaker) = self.speaker.clone() else {
                        continue;
                    };
                    let (tx, rx) = oneshot::channel();
                    let voice = self.voice.clone();
                    let rate = self.rate;
                    std::thread::spawn(move || {
                        let _ = tx.send(speaker.synthesize(&text, &voice, rate));
                    });
                    ctx.spawn(rx, move |me, result, ctx| {
                        let audio = match result {
                            Ok(Ok(audio)) => Some(Arc::new(audio)),
                            Ok(Err(error)) => {
                                log::warn!("Speech synthesis failed: {error}");
                                None
                            }
                            Err(_) => None,
                        };
                        let actions = me.controller.synthesized(reply, seq, audio, Instant::now());
                        me.apply(actions, ctx);
                    });
                }
                PlaybackAction::Play { reply, seq, audio } => {
                    self.refresh_echo_cancellation();
                    if let Some(output) = &self.output {
                        let finished_tx = self.finished_tx.clone();
                        output.play(&audio.samples, audio.sample_rate, move || {
                            let _ = finished_tx.unbounded_send((reply, seq));
                        });
                    }
                }
                PlaybackAction::StopPlayback => {
                    if let Some(output) = &self.output {
                        output.stop();
                    }
                }
                PlaybackAction::Interrupted { reply } => {
                    log::info!("Voice conversation reply {reply} interrupted by the user");
                }
            }
        }
        self.schedule_approval_timeout(ctx);
        if self.controller.state() != before {
            ctx.emit(ConversationEvent::StateChanged);
            ctx.notify();
        }
    }
}

/// The byte length of the longest common prefix of `a` and `b`, on a char boundary.
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or_else(|| a.len().min(b.len()), |((index, _), _)| index)
}

impl Entity for ConversationSession {
    type Event = ConversationEvent;
}

impl SingletonEntity for ConversationSession {}

/// Turns conversation mode on or off. Turning it on first makes sure the speech output model is
/// installed (asking the user to download it) and turns on hands-free input.
pub fn toggle_conversation(window_id: WindowId, ctx: &mut AppContext) {
    if !is_conversation_enabled() {
        return;
    }
    let session = ConversationSession::handle(ctx);
    if session.as_ref(ctx).is_active() {
        session.update(ctx, |session, ctx| session.stop(ctx));
        return;
    }
    if !ensure_speech_output_ready(window_id, ctx) {
        return;
    }
    if !HandsFreeSession::as_ref(ctx).is_active() {
        toggle_hands_free(window_id, ctx);
        if !HandsFreeSession::as_ref(ctx).is_active() {
            return;
        }
    }
    if let Err(message) = session.update(ctx, |session, ctx| session.start(window_id, ctx)) {
        ToastStack::handle(ctx).update(ctx, |stack, ctx| {
            stack.add_ephemeral_toast(DismissibleToast::error(message), window_id, ctx);
        });
    }
}

#[cfg(test)]
#[path = "conversation_tests.rs"]
mod tests;
