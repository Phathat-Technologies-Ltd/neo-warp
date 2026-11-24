//! Voice conversation mode: hands-free input plus agent replies spoken aloud on this computer.
//! Synthesis runs in the client, one sentence ahead of playback; the embedded server only does
//! speech-to-text.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::channel::{mpsc, oneshot};
use instant::Instant;
use local_transcription::Audio;
use local_transcription::speech::approval::ApprovalReply;
use local_transcription::speech::chunker::ChunkerOptions;
use local_transcription::speech::playback::{
    BARGE_IN_POLL, BARGE_IN_VERDICT_TIMEOUT, BargeInSpeaker, ConversationController,
    ConversationState, FinalOutcome, PlaybackAction, is_headphone_device_name,
};
use local_transcription::speech::{
    KokoroSpeaker, PocketSpeaker, Speaker, TinyVocaSpeaker, VoiceId, VoiceInfo,
};
use voice_input::SpeechOutput;
use warpui::r#async::Timer;
use warpui::{AppContext, Entity, EntityId, ModelContext, ModelHandle, SingletonEntity, WindowId};

use super::conversation_text::{
    action_announcement, reply_actions, reply_has_spoken_summary, spoken_action_prompt,
    spoken_reply_text,
};
use super::hands_free::{HandsFreeEvent, HandsFreeSession, toggle_hands_free};
use super::local_model::{
    LocalVoiceModelEvent, SpeechOutputEngine, SpeechOutputModel, ensure_speech_output_ready,
    is_conversation_enabled, speech_output_store,
};
use super::setup::{VoiceEntry, ensure_voice_ready};
use crate::ai::agent::api::VOICE_CONVERSATION_ACTIVE;
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent::{AIAgentActionId, CancellationReason};
use crate::ai::blocklist::{
    BlocklistAIActionModel, BlocklistAIHistoryEvent, BlocklistAIHistoryModel,
};
use crate::settings::{AISettings, AISettingsChangedEvent};
use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;

pub enum ConversationEvent {
    StateChanged,
}

/// Whether moving the hands-free target to `target` moves which pane's replies are read.
pub fn surface_changes(current: Option<EntityId>, target: Option<EntityId>) -> bool {
    target.is_some() && current != target
}

/// The idle timeout set in minutes; 0 never times out.
pub fn idle_timeout(minutes: u32) -> Option<Duration> {
    (minutes > 0).then(|| Duration::from_secs(u64::from(minutes) * 60))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleCheck {
    Expired,
    /// Not idle long enough yet; check again at this time.
    CheckAt(Instant),
}

pub fn check_idle(last_activity: Instant, timeout: Duration, now: Instant) -> IdleCheck {
    let deadline = last_activity + timeout;
    if now >= deadline {
        IdleCheck::Expired
    } else {
        IdleCheck::CheckAt(deadline)
    }
}

/// The toast shown when conversation mode turns itself off.
pub fn idle_ended_message(minutes: u32) -> String {
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    format!("Voice conversation ended after {minutes} {unit} without speech")
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
    /// Whether the reply has a `<spoken>` summary, which covers its tool calls.
    has_spoken: bool,
    /// How many of the reply's tool calls were already seen.
    announced_actions: usize,
}

pub struct ConversationSession {
    controller: ConversationController<Arc<Audio>, AIAgentActionId>,
    speaker: Option<Arc<dyn Speaker>>,
    /// The engine `speaker` runs.
    engine: SpeechOutputEngine,
    /// The engine the settings asked for; differs from `engine` while falling back or waiting for
    /// its model to download.
    requested_engine: SpeechOutputEngine,
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
    /// When the approval announcement timer already waiting will fire.
    approval_due_timer_at: Option<Instant>,
    finished_tx: mpsc::UnboundedSender<(u64, u64)>,
    /// The last user speech, prompt, agent reply, or approval, which the idle timeout counts from.
    last_activity: Instant,
    /// Counts idle timers so only the newest one acts.
    idle_timer_generation: u64,
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
        ctx.spawn_stream_local(
            voice_input::backend_changes(),
            |me, (), ctx| {
                if me.is_active() {
                    me.refresh_echo_cancellation();
                    ctx.notify();
                }
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
        ctx.subscribe_to_model(&AISettings::handle(ctx), |me, _, event, ctx| {
            if me.is_active() {
                me.schedule_idle_check(ctx);
            }
            let engine = SpeechOutputEngine::from_setting(
                &AISettings::as_ref(ctx).conversation_speech_engine,
            );
            SpeechOutputModel::handle(ctx).update(ctx, |model, ctx| model.set_engine(engine, ctx));
            if me.is_active() && is_speech_setting_event(event) {
                me.apply_speech_settings(ctx);
            }
        });
        ctx.subscribe_to_model(&SpeechOutputModel::handle(ctx), |me, model, event, ctx| {
            let LocalVoiceModelEvent::StateChanged = event;
            if me.is_active()
                && me.engine != me.requested_engine
                && me.requested_engine.spec().is_some()
                && model.as_ref(ctx).state().is_installed()
            {
                log::info!(
                    "Voice conversation: {:?} finished downloading; switching to it",
                    me.requested_engine
                );
                me.use_engine(me.requested_engine, ctx);
            }
        });
        Self {
            controller: ConversationController::default(),
            speaker: None,
            engine: SpeechOutputEngine::Kokoro,
            requested_engine: SpeechOutputEngine::Kokoro,
            voice: VoiceId::new(""),
            rate: 1.0,
            output: None,
            window_id: None,
            surface: None,
            reply: None,
            approval: None,
            approval_timeout_at: None,
            approval_due_timer_at: None,
            finished_tx,
            last_activity: Instant::now(),
            idle_timer_generation: 0,
        }
    }

    /// Counts user speech, prompts, agent replies, and approvals as activity for the idle timeout.
    fn note_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    fn idle_timeout_minutes(&self, ctx: &AppContext) -> u32 {
        *AISettings::as_ref(ctx).conversation_idle_timeout_minutes
    }

    /// Starts a timer for when conversation mode will have been idle for the timeout, replacing
    /// any earlier one.
    fn schedule_idle_check(&mut self, ctx: &mut ModelContext<Self>) {
        self.idle_timer_generation += 1;
        let Some(timeout) = idle_timeout(self.idle_timeout_minutes(ctx)) else {
            return;
        };
        let generation = self.idle_timer_generation;
        let wait = (self.last_activity + timeout).saturating_duration_since(Instant::now());
        ctx.spawn(Timer::after(wait), move |me, _, ctx| {
            if me.idle_timer_generation == generation {
                me.check_idle(ctx);
            }
        });
    }

    fn check_idle(&mut self, ctx: &mut ModelContext<Self>) {
        if !self.is_active() {
            return;
        }
        let minutes = self.idle_timeout_minutes(ctx);
        let Some(timeout) = idle_timeout(minutes) else {
            return;
        };
        // The agent working, speaking, or waiting on an approval isn't idleness.
        let agent_busy = matches!(
            self.controller.state(),
            ConversationState::Thinking | ConversationState::Speaking
        ) || self.controller.approvals().pending().is_some();
        if agent_busy {
            self.note_activity();
        }
        match check_idle(self.last_activity, timeout, Instant::now()) {
            IdleCheck::CheckAt(_) => self.schedule_idle_check(ctx),
            IdleCheck::Expired => {
                log::info!("Voice conversation: ending after {minutes} idle minutes");
                if let Some(window_id) = self.window_id {
                    let message = idle_ended_message(minutes);
                    ToastStack::handle(ctx).update(ctx, |stack, ctx| {
                        stack.add_ephemeral_toast(
                            DismissibleToast::default(message),
                            window_id,
                            ctx,
                        );
                    });
                }
                self.stop(ctx);
            }
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
        let wanted = WantedSpeech::from_settings(settings);
        let engine = wanted.engine;
        let speaker = new_speaker(engine);
        self.engine = engine;
        self.requested_engine = engine;
        self.voice = choose_voice(&speaker.voices(), &wanted.voice)
            .ok_or_else(|| "The speech output model has no voices".to_owned())?;
        self.rate = wanted.rate;
        let options = ChunkerOptions {
            read_code_summaries: *settings.conversation_read_code_summaries,
        };
        // With system echo cancellation the microphone is already cleaned and speech plays on the
        // plain path. Otherwise the microphone and the speaker must both be on the echo-cancelling
        // unit, so hands-free capture moves onto it as well.
        super::apply_system_echo_cancellation_setting(ctx);
        voice_input::set_voice_processing_wanted(*settings.conversation_echo_cancellation);
        let output = SpeechOutput::open().map_err(|error| {
            voice_input::set_voice_processing_wanted(false);
            format!("Couldn't open the speaker: {error:#}")
        })?;
        let headphones =
            output.reports_headphones() || is_headphone_device_name(output.device_name());
        self.output = Some(output);
        self.warm_up(engine, &speaker, ctx);
        self.speaker = Some(speaker);
        self.window_id = Some(window_id);
        self.controller.start(options);
        self.controller.set_headphones(headphones);
        self.refresh_echo_cancellation();
        self.note_activity();
        self.schedule_idle_check(ctx);
        VOICE_CONVERSATION_ACTIVE.store(true, Ordering::Relaxed);
        ctx.emit(ConversationEvent::StateChanged);
        ctx.notify();
        Ok(())
    }

    /// Loads `speaker`'s model now, since that takes seconds, rather than on the first sentence.
    fn warm_up(
        &self,
        engine: SpeechOutputEngine,
        speaker: &Arc<dyn Speaker>,
        ctx: &mut ModelContext<Self>,
    ) {
        let warm = speaker.clone();
        let voice = self.voice.clone();
        let (tx, rx) = oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(warm.synthesize("Ready.", &voice, 1.0).err());
        });
        ctx.spawn(rx, move |me, error, ctx| {
            if let Ok(Some(error)) = error {
                me.speech_model_failed(engine, error.to_string(), ctx);
            }
        });
    }

    /// Applies changed speech settings to the running conversation: a new engine replaces the
    /// speaker, while a new voice or rate takes effect from the next sentence.
    fn apply_speech_settings(&mut self, ctx: &mut ModelContext<Self>) {
        let wanted = WantedSpeech::from_settings(AISettings::as_ref(ctx));
        let voices = self
            .speaker
            .as_ref()
            .map(|speaker| speaker.voices())
            .unwrap_or_default();
        match speech_setting_change(
            self.requested_engine,
            self.engine,
            &self.voice,
            self.rate,
            &wanted,
            &voices,
        ) {
            SpeechSettingChange::Unchanged => {}
            SpeechSettingChange::Engine(engine) => self.switch_engine(engine, ctx),
            SpeechSettingChange::Voice { voice, rate } => {
                log::info!(
                    "Voice conversation: speaking with voice {} at {rate:.2}x from the next \
                     sentence",
                    voice.as_str()
                );
                self.voice = voice;
                self.rate = rate;
            }
        }
    }

    /// Stops the current reply and replaces the speaker with one for `engine`, asking to download
    /// its model if needed and speaking with the fallback engine meanwhile.
    fn switch_engine(&mut self, engine: SpeechOutputEngine, ctx: &mut ModelContext<Self>) {
        log::info!(
            "Voice conversation: switching speech output from {:?} to {engine:?}",
            self.engine
        );
        self.requested_engine = engine;
        self.stop_speaking(ctx);
        self.speaker = None;
        let ready = engine.spec().is_none()
            || self
                .window_id
                .is_some_and(|window_id| ensure_speech_output_ready(window_id, ctx));
        if ready {
            self.use_engine(engine, ctx);
            return;
        }
        match engine.fallback() {
            Some(fallback) if new_speaker(fallback).is_ready() => {
                log::info!(
                    "Voice conversation: speaking with {fallback:?} until {engine:?} is downloaded"
                );
                self.use_engine(fallback, ctx);
            }
            Some(_) | None => {
                log::info!("Voice conversation: replies stay silent until {engine:?} is downloaded")
            }
        }
    }

    /// Speaks with a new speaker for `engine`, using the chosen voice when `engine` is the one
    /// the settings asked for.
    fn use_engine(&mut self, engine: SpeechOutputEngine, ctx: &mut ModelContext<Self>) {
        let speaker = new_speaker(engine);
        let wanted = WantedSpeech::from_settings(AISettings::as_ref(ctx));
        let chosen = if engine == self.requested_engine {
            wanted.voice.as_str()
        } else {
            ""
        };
        if let Some(voice) = choose_voice(&speaker.voices(), chosen) {
            self.voice = voice;
        }
        self.rate = if engine == self.requested_engine {
            wanted.rate
        } else {
            1.0
        };
        self.engine = engine;
        self.warm_up(engine, &speaker, ctx);
        self.speaker = Some(speaker);
        ctx.notify();
    }

    /// Switches to the fallback engine when `engine`'s model couldn't load, so replies are still
    /// read aloud.
    fn speech_model_failed(
        &mut self,
        engine: SpeechOutputEngine,
        error: String,
        ctx: &mut ModelContext<Self>,
    ) {
        log::warn!("Couldn't load the speech output model: {error}");
        let Some(fallback) = engine.fallback() else {
            return;
        };
        if !self.is_active() || self.engine != engine {
            return;
        }
        let speaker = new_speaker(fallback);
        let message = if speaker.is_ready() {
            log::warn!("Falling back from {engine:?} to {fallback:?} for speech output");
            self.voice = choose_voice(&speaker.voices(), "").unwrap_or_else(|| self.voice.clone());
            self.rate = 1.0;
            self.speaker = Some(speaker);
            self.engine = fallback;
            format!(
                "Couldn't load {}, so replies are read with {} instead.",
                engine.display_name(),
                fallback.display_name()
            )
        } else {
            format!(
                "Couldn't load {}. Download {} in Settings to have replies read aloud.",
                engine.display_name(),
                fallback.display_name()
            )
        };
        if let Some(window_id) = self.window_id {
            ToastStack::handle(ctx).update(ctx, |stack, ctx| {
                stack.add_ephemeral_toast(DismissibleToast::error(message), window_id, ctx);
            });
        }
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
        self.idle_timer_generation += 1;
        ctx.emit(ConversationEvent::StateChanged);
        ctx.notify();
        // Conversation mode off means the microphone off, whoever turned hands-free on.
        let hands_free = HandsFreeSession::handle(ctx);
        if hands_free.as_ref(ctx).is_active() {
            hands_free.update(ctx, |hands_free, ctx| hands_free.stop(ctx));
        }
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
        self.note_activity();
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
        self.note_activity();
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
                })
            }
        };
        reply.has_spoken |= has_spoken;
        let common = common_prefix_len(&reply.fed, &text);
        let delta = text[common..].to_owned();
        reply.fed = text;
        let mut immediate = Vec::new();
        for announcement in announcements.into_iter().skip(reply.announced_actions) {
            reply.announced_actions += 1;
            if reply.has_spoken || finished {
                continue;
            }
            immediate.extend(announcement);
        }
        let mut actions = self.controller.reply_text(&delta);
        for line in immediate {
            actions.extend(self.controller.speak(line));
        }
        if finished {
            actions.extend(self.controller.reply_finished());
        }
        self.apply(actions, ctx);
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
        self.note_activity();
        let Some((command, needs_screen)) = action_model
            .as_ref(ctx)
            .get_pending_action_by_id(&action_id)
            .map(|action| spoken_action_prompt(&action.action))
        else {
            return;
        };
        self.approval = Some(ApprovalTarget {
            action_model,
            conversation_id,
        });
        self.controller
            .approval_requested(action_id, &command, needs_screen, Instant::now());
        self.schedule_approval_due(ctx);
    }

    /// Starts a timer for when the waiting approval requests are due to be announced.
    fn schedule_approval_due(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(due) = self.controller.approval_due_at() else {
            return;
        };
        if self.approval_due_timer_at.is_some_and(|at| at <= due) {
            return;
        }
        self.approval_due_timer_at = Some(due);
        ctx.spawn(
            Timer::after(due.saturating_duration_since(Instant::now())),
            |me, _, ctx| {
                me.approval_due_timer_at = None;
                let actions = me.controller.check_approval_due(Instant::now());
                me.apply(actions, ctx);
                me.schedule_approval_due(ctx);
            },
        );
    }

    /// The action stopped waiting for approval (answered on screen, run, or cancelled).
    pub fn approval_resolved(&mut self, action_id: &AIAgentActionId, ctx: &mut ModelContext<Self>) {
        self.note_activity();
        let actions = self.controller.approval_resolved(action_id, Instant::now());
        if self.controller.approvals().pending().is_none() {
            self.approval_timeout_at = None;
        }
        self.apply(actions, ctx);
    }

    /// Lets conversation mode handle a final utterance before hands-free applies it. Returns
    /// whether it was consumed ("stop", or an approval answer) and must not reach the input.
    pub fn intercept_final(
        &mut self,
        text: &str,
        speaker_verified: bool,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        self.refresh_echo_cancellation();
        let (outcome, actions) =
            self.controller
                .on_final_from(text, speaker_verified, Instant::now());
        self.apply(actions, ctx);
        if !matches!(outcome, FinalOutcome::NotConsumed) {
            log::info!(
                "Voice conversation took the final ({})",
                outcome_label(&outcome)
            );
        }
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

    /// The VAD heard speech start; talking over the reply for long enough stops it. With
    /// `verify_speaker`, it only ducks the reply until [`Self::barge_in_verdict`].
    pub fn speech_started(&mut self, verify_speaker: bool, ctx: &mut ModelContext<Self>) {
        if !self.is_active() {
            return;
        }
        self.note_activity();
        self.refresh_echo_cancellation();
        self.controller.set_verify_barge_in(verify_speaker);
        self.controller.speech_started(Instant::now());
        self.poll_barge_in(ctx);
    }

    /// Whether a ducked reply is waiting for the speaker check of the speech over it.
    pub fn wants_barge_in_verdict(&self) -> bool {
        self.is_active() && self.controller.is_barge_in_pending()
    }

    /// Reports whether the speech over the reply is the enrolled user's.
    pub fn barge_in_verdict(&mut self, verdict: BargeInSpeaker, ctx: &mut ModelContext<Self>) {
        let actions = self.controller.barge_in_verdict(verdict, Instant::now());
        self.apply(actions, ctx);
    }

    /// Reports a partial transcript of the speech over a ducked reply.
    pub fn barge_in_transcript(&mut self, text: &str, ctx: &mut ModelContext<Self>) {
        if !self.is_active() {
            return;
        }
        let actions = self.controller.barge_in_transcript(text, Instant::now());
        self.apply(actions, ctx);
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
                    let engine = self.engine;
                    std::thread::spawn(move || {
                        let _ = tx.send(speaker.synthesize(&text, &voice, rate));
                    });
                    ctx.spawn(rx, move |me, result, ctx| {
                        let audio = match result {
                            Ok(Ok(audio)) => Some(Arc::new(audio)),
                            Ok(Err(error)) => {
                                log::warn!("Speech synthesis failed: {error}");
                                // The system synthesizer has no model to load up front, so its
                                // failures surface here rather than in the warm-up.
                                if engine == SpeechOutputEngine::Apple {
                                    me.speech_model_failed(engine, error.to_string(), ctx);
                                }
                                None
                            }
                            Err(_) => None,
                        };
                        let actions = me.controller.synthesized(reply, seq, audio, Instant::now());
                        me.apply(actions, ctx);
                    });
                }
                PlaybackAction::Play { reply, seq, audio } => {
                    if let Some(output) = &self.output {
                        let finished_tx = self.finished_tx.clone();
                        output.play(&audio.samples, audio.sample_rate, move || {
                            let _ = finished_tx.unbounded_send((reply, seq));
                        });
                    }
                    // Playing may have moved the speaker onto the echo-cancelling unit.
                    self.refresh_echo_cancellation();
                }
                PlaybackAction::StopPlayback => {
                    if let Some(output) = &self.output {
                        output.stop();
                    }
                }
                PlaybackAction::Interrupted { reply } => {
                    log::info!("Voice conversation reply {reply} interrupted by the user");
                }
                PlaybackAction::Duck { gain } => {
                    if let Some(output) = &self.output {
                        output.set_gain(gain);
                    }
                    ctx.spawn(Timer::after(BARGE_IN_VERDICT_TIMEOUT), |me, _, ctx| {
                        let actions = me.controller.check_barge_in(Instant::now());
                        me.apply(actions, ctx);
                    });
                }
                PlaybackAction::Restore => {
                    if let Some(output) = &self.output {
                        output.set_gain(1.0);
                    }
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

/// Turns conversation mode on or off. Turning it on first makes sure every model it needs is
/// installed, asking once to download the missing ones and starting when they're in, and turns on
/// hands-free input. `surface` is the terminal surface whose controls were used: while
/// conversation mode runs in another pane, it moves there instead of stopping.
pub fn toggle_conversation(window_id: WindowId, surface: Option<EntityId>, ctx: &mut AppContext) {
    if !is_conversation_enabled() {
        return;
    }
    let session = ConversationSession::handle(ctx);
    if session.as_ref(ctx).is_active() {
        let hands_free = HandsFreeSession::handle(ctx);
        match surface {
            Some(surface) if !hands_free.as_ref(ctx).is_target(window_id, surface) => {
                hands_free.update(ctx, |hands_free, ctx| {
                    hands_free.focus_target(window_id, surface, ctx)
                });
            }
            Some(_) | None => session.update(ctx, |session, ctx| session.stop(ctx)),
        }
        return;
    }
    if !ensure_voice_ready(VoiceEntry::Conversation, window_id, ctx) {
        return;
    }
    let started_hands_free = !HandsFreeSession::as_ref(ctx).is_active();
    if started_hands_free {
        toggle_hands_free(window_id, ctx);
        if !HandsFreeSession::as_ref(ctx).is_active() {
            return;
        }
    }
    if let Some(surface) = surface {
        HandsFreeSession::handle(ctx).update(ctx, |hands_free, ctx| {
            hands_free.focus_target(window_id, surface, ctx)
        });
    }
    if let Err(message) = session.update(ctx, |session, ctx| session.start(window_id, ctx)) {
        // Hands-free turned on only for this conversation goes off with it.
        if started_hands_free {
            HandsFreeSession::handle(ctx).update(ctx, |hands_free, ctx| hands_free.stop(ctx));
        }
        ToastStack::handle(ctx).update(ctx, |stack, ctx| {
            stack.add_ephemeral_toast(DismissibleToast::error(message), window_id, ctx);
        });
    }
}

/// Whether `event` changes how conversation replies are spoken.
fn is_speech_setting_event(event: &AISettingsChangedEvent) -> bool {
    matches!(
        event,
        AISettingsChangedEvent::ConversationSpeechEngine { .. }
            | AISettingsChangedEvent::ConversationVoice { .. }
            | AISettingsChangedEvent::ConversationSpeechRatePercent { .. }
    )
}

/// The speech output the conversation settings ask for.
#[derive(Debug, Clone, PartialEq)]
struct WantedSpeech {
    engine: SpeechOutputEngine,
    /// The chosen voice id; empty or unknown means the engine's default.
    voice: String,
    rate: f32,
}

impl WantedSpeech {
    fn from_settings(settings: &AISettings) -> Self {
        let engine = SpeechOutputEngine::from_setting(&settings.conversation_speech_engine);
        Self {
            engine,
            voice: settings.conversation_voice.clone(),
            rate: if engine.has_voice_and_rate_controls() {
                settings.conversation_speech_rate()
            } else {
                1.0
            },
        }
    }
}

/// How a running conversation follows a change to its speech settings.
#[derive(Debug, PartialEq)]
enum SpeechSettingChange {
    Unchanged,
    /// Replace the speaker with one for this engine.
    Engine(SpeechOutputEngine),
    /// Keep the speaker; speak the next sentence with this voice and rate.
    Voice {
        voice: VoiceId,
        rate: f32,
    },
}

/// What a conversation speaking with `engine`, `voice`, and `rate` for `requested_engine` must do
/// to match `wanted`; `voices` are the current speaker's. While a fallback engine stands in for
/// `requested_engine`, voice and rate changes wait for the requested engine.
fn speech_setting_change(
    requested_engine: SpeechOutputEngine,
    engine: SpeechOutputEngine,
    voice: &VoiceId,
    rate: f32,
    wanted: &WantedSpeech,
    voices: &[VoiceInfo],
) -> SpeechSettingChange {
    if wanted.engine != requested_engine {
        return SpeechSettingChange::Engine(wanted.engine);
    }
    if engine != requested_engine {
        return SpeechSettingChange::Unchanged;
    }
    let next_voice = choose_voice(voices, &wanted.voice).unwrap_or_else(|| voice.clone());
    if next_voice == *voice && wanted.rate == rate {
        SpeechSettingChange::Unchanged
    } else {
        SpeechSettingChange::Voice {
            voice: next_voice,
            rate: wanted.rate,
        }
    }
}

fn new_speaker(engine: SpeechOutputEngine) -> Arc<dyn Speaker> {
    let store = || speech_output_store(engine).expect("engines other than Apple download a model");
    match engine {
        SpeechOutputEngine::Kokoro => Arc::new(KokoroSpeaker::new(store())),
        SpeechOutputEngine::TinyVoca => Arc::new(TinyVocaSpeaker::new(store())),
        SpeechOutputEngine::Pocket => Arc::new(PocketSpeaker::new(store())),
        #[cfg(target_os = "macos")]
        SpeechOutputEngine::Apple => Arc::new(local_transcription::speech::AppleSpeaker::new(
            super::local_model::CONVERSATION_LANGUAGE,
        )),
        #[cfg(not(target_os = "macos"))]
        SpeechOutputEngine::Apple => Arc::new(KokoroSpeaker::new(
            speech_output_store(SpeechOutputEngine::Kokoro).expect("Kokoro downloads a model"),
        )),
    }
}

/// The voice named `chosen`, or the engine's default when it doesn't offer that one.
fn choose_voice(voices: &[VoiceInfo], chosen: &str) -> Option<VoiceId> {
    voices
        .iter()
        .find(|voice| voice.id.as_str() == chosen)
        .or_else(|| voices.first())
        .map(|voice| voice.id.clone())
}

fn outcome_label<Id>(outcome: &FinalOutcome<Id>) -> &'static str {
    match outcome {
        FinalOutcome::NotConsumed => "not consumed",
        FinalOutcome::Stopped => "stop phrase",
        FinalOutcome::Ignored => "ignored approval answer",
        FinalOutcome::Echo => "echo",
        FinalOutcome::Answered(..) => "approval answer",
    }
}

#[cfg(test)]
#[path = "conversation_tests.rs"]
mod tests;
