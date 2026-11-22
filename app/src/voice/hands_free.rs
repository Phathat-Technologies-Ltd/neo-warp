//! Hands-free voice input: the microphone stays open, the embedded server splits speech into
//! utterances (`/ai/transcribe/stream`), and transcripts flow into the focused input. Audio stays
//! on the device and is never written to disk.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::channel::mpsc;
use futures::{SinkExt as _, StreamExt as _};
use instant::Instant;
use local_transcription::denoise::DenoiseStage;
use local_transcription::speaker::Sensitivity;
use local_transcription::stream::{ClientMessage, ServerMessage, VadOptions};
use voice_input::{LiveCapture, LiveChunk};
use warpui::r#async::Timer;
use warpui::platform::MicrophoneAccessState;
use warpui::windowing::state::ApplicationStage;
use warpui::windowing::{StateEvent, WindowManager};
use warpui::{AppContext, Entity, EntityId, ModelContext, SingletonEntity, WindowId};
use websocket::WebsocketMessage as _;

use super::conversation::ConversationSession;
use super::footer_controls::DictationMode;
use super::hands_free_controller::{
    HandsFreeController, HandsFreeEffect, HandsFreeOptions, HandsFreeState, VoiceTarget,
};
use super::local_model::{ensure_ready_to_record, is_hands_free_enabled};
use super::speaker_gate::{AudioTap, GateStep, SpeakerGate};
use super::speaker_id::SpeakerProfile;
use crate::server::server_api::ServerApiProvider;
use crate::settings::AISettings;
use crate::system::{SystemStats, SystemStatsEvent};
use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;

/// How often a held auto-send re-checks whether the reply has gone quiet.
const ECHO_RECHECK: Duration = Duration::from_millis(200);

pub enum HandsFreeEvent {
    /// The state changed; every input refreshes its indicator.
    StateChanged,
    /// An edit for the focused input of the session's window.
    Effect(HandsFreeEffect),
}

enum SocketUpdate {
    Message(ServerMessage),
    Closed(Option<String>),
}

#[derive(Default)]
pub struct HandsFreeSession {
    controller: HandsFreeController,
    capture: Option<LiveCapture>,
    /// Counts sessions so updates from an ended one are dropped.
    generation: u64,
    /// The window whose focused input receives effects, so one transcript is applied once.
    window_id: Option<WindowId>,
    /// The one input (by terminal surface) that shows the session's state and receives its text.
    target: VoiceTarget,
    /// Shared with the socket writer, which drops audio while the user types.
    audio_paused: Arc<AtomicBool>,
    /// Whether finals must match the user's voiceprint, decided when the session starts.
    verify_speaker: bool,
    sensitivity: Sensitivity,
    gate: SpeakerGate,
    /// Recent audio sent to the server, which speaker checks embed; filled only while verifying.
    tap: Arc<Mutex<AudioTap>>,
    /// Until when the indicator says an utterance was ignored.
    rejected_until: Option<Instant>,
    /// What a click on a footer mic button does; shared so every footer agrees.
    dictation_mode: DictationMode,
    /// Whether the session follows app activation and system sleep.
    watching_app: bool,
}

/// How long "Not you — ignored" shows.
const REJECTION_NOTICE: Duration = Duration::from_secs(3);

impl HandsFreeSession {
    pub fn state(&self) -> HandsFreeState {
        self.controller.state()
    }

    /// Whether the session is on, including while paused because Warp isn't the active app.
    pub fn is_active(&self) -> bool {
        self.controller.state().is_on()
    }

    pub fn dictation_mode(&self) -> DictationMode {
        self.dictation_mode
    }

    pub fn set_dictation_mode(&mut self, mode: DictationMode, ctx: &mut ModelContext<Self>) {
        if self.dictation_mode == mode {
            return;
        }
        self.dictation_mode = mode;
        ctx.emit(HandsFreeEvent::StateChanged);
        ctx.notify();
    }

    /// Whether the indicator should say an utterance was ignored for not matching the user.
    pub fn showing_rejection(&self) -> bool {
        self.rejected_until
            .is_some_and(|until| Instant::now() < until)
    }

    /// The window whose focused input applies effects.
    pub fn window_id(&self) -> Option<WindowId> {
        self.window_id
    }

    /// Whether the input of `surface` in `window_id` is the session's target: only it shows
    /// the live state and receives transcripts.
    pub fn is_target(&self, window_id: WindowId, surface: EntityId) -> bool {
        self.is_active() && self.target.is(window_id, surface)
    }

    /// The terminal surface whose input is the target, if one has claimed it.
    pub fn target_surface(&self) -> Option<EntityId> {
        self.target.surface()
    }

    /// Makes the focused input of `surface` in `window_id` the target, moving the indicator and
    /// transcripts to it. The microphone session itself is unchanged.
    pub fn focus_target(
        &mut self,
        window_id: WindowId,
        surface: EntityId,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.is_active() || !self.target.focus(window_id, surface) {
            return;
        }
        self.window_id = Some(window_id);
        log::info!("Hands-free: target moved to the focused input");
        ctx.emit(HandsFreeEvent::StateChanged);
        ctx.notify();
    }

    pub fn user_typed(&mut self, ctx: &mut ModelContext<Self>) {
        self.controller.user_typed();
        self.sync_audio_paused();
        ctx.notify();
    }

    pub fn input_reset(&mut self, ctx: &mut ModelContext<Self>) {
        self.controller.input_reset();
        self.sync_audio_paused();
        ctx.notify();
    }

    fn sync_audio_paused(&self) {
        self.audio_paused
            .store(self.controller.is_paused(), Ordering::Relaxed);
    }

    fn show_error(&self, message: String, ctx: &mut ModelContext<Self>) {
        let Some(window_id) = self.window_id else {
            return;
        };
        ToastStack::handle(ctx).update(ctx, |stack, ctx| {
            stack.add_ephemeral_toast(DismissibleToast::error(message), window_id, ctx);
        });
    }

    /// Passes a server message through the speaker gate, which holds finals back until their
    /// speaker is checked.
    fn on_server_message(&mut self, message: ServerMessage, ctx: &mut ModelContext<Self>) {
        if self.controller.state() == HandsFreeState::Paused {
            return;
        }
        let now = self.tap_total();
        let verify = self.should_verify(ctx);
        let steps = self.gate.push(message, now, verify);
        self.run_gate_steps(steps, ctx);
    }

    /// Whether a final arriving now is checked against the voiceprint. Finals heard while the
    /// assistant may be echoing skip the check, since conversation mode drops them anyway.
    fn should_verify(&self, ctx: &ModelContext<Self>) -> bool {
        self.verify_speaker
            && !self.controller.is_paused()
            && !ConversationSession::as_ref(ctx).is_echo_window()
    }

    fn tap_total(&self) -> u64 {
        self.tap.lock().map_or(0, |tap| tap.total())
    }

    fn run_gate_steps(&mut self, steps: Vec<GateStep>, ctx: &mut ModelContext<Self>) {
        for step in steps {
            match step {
                GateStep::Deliver(message) => self.handle_message(message, ctx),
                GateStep::Verify(request) => {
                    let samples = self
                        .tap
                        .lock()
                        .map(|tap| tap.slice(request.range))
                        .unwrap_or_default();
                    let sensitivity = self.sensitivity;
                    let verdict = SpeakerProfile::handle(ctx)
                        .update(ctx, |profile, _| profile.verify(samples, sensitivity));
                    let generation = self.generation;
                    ctx.spawn(verdict, move |me, verdict, ctx| {
                        if me.generation != generation {
                            return;
                        }
                        let now = me.tap_total();
                        let verify = me.should_verify(ctx);
                        let (steps, rejected) = me.gate.verdict(verdict, now, verify);
                        if rejected {
                            me.show_rejection(ctx);
                        }
                        me.run_gate_steps(steps, ctx);
                    });
                }
            }
        }
    }

    fn show_rejection(&mut self, ctx: &mut ModelContext<Self>) {
        self.rejected_until = Some(Instant::now() + REJECTION_NOTICE);
        ctx.emit(HandsFreeEvent::StateChanged);
        ctx.notify();
        ctx.spawn(Timer::after(REJECTION_NOTICE), |me, _, ctx| {
            if !me.showing_rejection() {
                me.rejected_until = None;
                ctx.emit(HandsFreeEvent::StateChanged);
                ctx.notify();
            }
        });
    }

    fn handle_message(&mut self, message: ServerMessage, ctx: &mut ModelContext<Self>) {
        if let ServerMessage::Error { message } = &message {
            self.show_error(format!("Hands-free voice input: {message}"), ctx);
        }
        let message = self.route_to_conversation(message, ctx);
        let before = self.controller.state();
        let is_transcript = matches!(
            message,
            ServerMessage::Partial { .. } | ServerMessage::Final { .. }
        );
        let effects = self.controller.on_message(message, Instant::now());
        if is_transcript && effects.is_empty() {
            log::info!(
                "Hands-free: transcript produced no edit (paused for typing: {}, state: {:?})",
                self.controller.is_paused(),
                self.controller.state()
            );
        }
        for effect in effects {
            ctx.emit(HandsFreeEvent::Effect(effect));
        }
        self.schedule_auto_send(ctx);
        if self.controller.state() != before {
            ctx.emit(HandsFreeEvent::StateChanged);
        }
        ctx.notify();
    }

    /// Tells conversation mode about speech so it can stop talking, and lets it take finals that
    /// answer it ("stop", or an approval answer), which then only clear their partial.
    fn route_to_conversation(
        &mut self,
        message: ServerMessage,
        ctx: &mut ModelContext<Self>,
    ) -> ServerMessage {
        let conversation = ConversationSession::handle(ctx);
        match message {
            ServerMessage::SpeechStart => {
                conversation.update(ctx, |conversation, ctx| conversation.speech_started(ctx));
                ServerMessage::SpeechStart
            }
            ServerMessage::SpeechEnd => {
                conversation.update(ctx, |conversation, _| conversation.speech_ended());
                ServerMessage::SpeechEnd
            }
            ServerMessage::Final { utterance, text } => {
                let consumed = !self.controller.is_paused()
                    && conversation.update(ctx, |conversation, ctx| {
                        conversation.intercept_final(&text, ctx)
                    });
                ServerMessage::Final {
                    utterance,
                    text: if consumed { String::new() } else { text },
                }
            }
            ServerMessage::Partial { utterance, text } => ServerMessage::Partial {
                utterance,
                text: if conversation.as_ref(ctx).is_echo_window() {
                    String::new()
                } else {
                    text
                },
            },
            message @ ServerMessage::Error { .. } => message,
        }
    }

    fn schedule_auto_send(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(at) = self.controller.auto_send_at() else {
            return;
        };
        let generation = self.generation;
        let delay = at.saturating_duration_since(Instant::now());
        ctx.spawn(Timer::after(delay), move |me, _, ctx| {
            if me.generation != generation {
                return;
            }
            if ConversationSession::as_ref(ctx).is_echo_window() {
                me.controller.defer_auto_send(Instant::now() + ECHO_RECHECK);
                me.schedule_auto_send(ctx);
                return;
            }
            if let Some(effect) = me.controller.tick(Instant::now()) {
                ctx.emit(HandsFreeEvent::Effect(effect));
            }
        });
    }

    fn start(&mut self, window_id: WindowId, ctx: &mut ModelContext<Self>) -> Result<(), String> {
        if matches!(
            ctx.microphone_access_state(),
            MicrophoneAccessState::Denied | MicrophoneAccessState::Restricted
        ) {
            return Err("Microphone access is denied".to_owned());
        }
        self.watch_app(ctx);
        self.window_id = Some(window_id);
        self.target = VoiceTarget::default();
        let settings = AISettings::as_ref(ctx);
        let options = HandsFreeOptions {
            auto_send_after: settings.hands_free_auto_send_after(),
            voice_commands: *settings.hands_free_voice_commands,
        };
        self.controller.start(options);
        if let Err(message) = self.open_microphone(ctx) {
            self.controller.stop();
            return Err(message);
        }
        log::info!(
            "Hands-free: session {} started (auto-send: {:?})",
            self.generation,
            options.auto_send_after
        );
        ctx.emit(HandsFreeEvent::StateChanged);
        ctx.notify();
        Ok(())
    }

    /// Pauses the session while Warp isn't the active app, resumes it when Warp is again, and
    /// ends it before the system sleeps.
    fn watch_app(&mut self, ctx: &mut ModelContext<Self>) {
        if self.watching_app {
            return;
        }
        self.watching_app = true;
        ctx.subscribe_to_model(&WindowManager::handle(ctx), |me, _, event, ctx| {
            let StateEvent::ValueChanged { current, previous } = event;
            if current.stage == previous.stage {
                return;
            }
            match current.stage {
                ApplicationStage::Active => me.resume(ctx),
                ApplicationStage::Inactive | ApplicationStage::Terminating => me.pause(ctx),
                ApplicationStage::Starting => {}
            }
        });
        ctx.subscribe_to_model(&SystemStats::handle(ctx), |me, _, event, ctx| match event {
            SystemStatsEvent::CpuWillSleep => {
                if me.is_active() {
                    log::info!("Hands-free: stopping because the system is going to sleep");
                    me.stop(ctx);
                }
            }
            SystemStatsEvent::CpuWasAwakened => {}
        });
    }

    /// Closes the microphone while Warp isn't the active app. The open utterance is discarded and
    /// nothing said meanwhile reaches the input.
    pub fn pause(&mut self, ctx: &mut ModelContext<Self>) {
        if !self.controller.state().is_mic_on() {
            return;
        }
        log::info!("Hands-free: pausing because Warp is no longer the active app");
        // Dropping the capture closes the microphone; the new generation drops the socket's
        // remaining messages, including the open utterance's final.
        self.capture = None;
        self.generation += 1;
        for effect in self.controller.pause() {
            ctx.emit(HandsFreeEvent::Effect(effect));
        }
        ctx.emit(HandsFreeEvent::StateChanged);
        ctx.notify();
    }

    /// Reopens the microphone after [`Self::pause`].
    pub fn resume(&mut self, ctx: &mut ModelContext<Self>) {
        if self.controller.state() != HandsFreeState::Paused {
            return;
        }
        log::info!("Hands-free: resuming because Warp is the active app again");
        if let Err(message) = self.open_microphone(ctx) {
            log::warn!("Hands-free: couldn't resume: {message}");
            self.show_error(message, ctx);
            self.stop(ctx);
            return;
        }
        self.controller.resume();
        ctx.emit(HandsFreeEvent::StateChanged);
        ctx.notify();
    }

    /// Opens the microphone and the transcription socket under a new generation.
    fn open_microphone(&mut self, ctx: &mut ModelContext<Self>) -> Result<(), String> {
        let (audio_tx, audio_rx) = mpsc::unbounded::<LiveChunk>();
        let (update_tx, update_rx) = mpsc::unbounded();
        let failure_tx = update_tx.clone();
        let settings = AISettings::as_ref(ctx);
        let mut denoise = DenoiseStage::new(*settings.voice_denoise);
        let sensitivity = Sensitivity::from_setting(&settings.voice_speaker_id_sensitivity);
        let verify_speaker = SpeakerProfile::as_ref(ctx).is_verifying(ctx);
        let tap = Arc::new(Mutex::new(AudioTap::default()));
        let capture_tap = verify_speaker.then(|| tap.clone());
        let capture = LiveCapture::start(
            move |mut chunk| {
                chunk.pcm = denoise.process(chunk.pcm);
                if chunk.pcm.is_empty() {
                    return;
                }
                if let Some(tap) = &capture_tap
                    && let Ok(mut tap) = tap.lock()
                {
                    tap.push_pcm(&chunk.pcm);
                }
                let _ = audio_tx.unbounded_send(chunk);
            },
            move |error| {
                let _ = failure_tx.unbounded_send(SocketUpdate::Closed(Some(error)));
            },
        )
        .map_err(|error| format!("Couldn't open the microphone: {error:#}"))?;
        self.generation += 1;
        self.capture = Some(capture);
        self.audio_paused = Arc::new(AtomicBool::new(self.controller.is_paused()));
        self.verify_speaker = verify_speaker;
        self.sensitivity = sensitivity;
        self.gate = SpeakerGate::default();
        self.tap = tap;
        self.rejected_until = None;
        let language = settings.voice_input_language_code().map(str::to_owned);

        let server_api = ServerApiProvider::as_ref(ctx).get();
        let generation = self.generation;
        ctx.spawn(
            run_socket(
                server_api,
                language,
                audio_rx,
                self.audio_paused.clone(),
                update_tx,
            ),
            |_, _, _| {},
        );
        ctx.spawn_stream_local(
            update_rx,
            move |me, update, ctx| {
                if me.generation != generation {
                    return;
                }
                match update {
                    SocketUpdate::Message(message) => me.on_server_message(message, ctx),
                    SocketUpdate::Closed(error) => {
                        if error.is_none() {
                            log::info!("Hands-free voice input stream closed");
                        }
                        if let Some(error) = error {
                            log::warn!("Hands-free voice input stopped: {error}");
                            me.show_error(format!("Hands-free voice input stopped: {error}"), ctx);
                        }
                        me.stop(ctx);
                    }
                }
            },
            |_, _| {},
        );
        Ok(())
    }

    pub fn stop(&mut self, ctx: &mut ModelContext<Self>) {
        // Dropping the capture closes the microphone and ends the audio stream, which ends the
        // socket.
        self.capture = None;
        self.generation += 1;
        self.gate = SpeakerGate::default();
        self.rejected_until = None;
        if let Ok(mut tap) = self.tap.lock() {
            *tap = AudioTap::default();
        }
        self.target = VoiceTarget::default();
        if self.controller.state().is_on() {
            self.controller.stop();
            ctx.emit(HandsFreeEvent::StateChanged);
            ctx.notify();
        }
    }
}

/// Connects to the server, then streams audio to it until the session ends.
async fn run_socket(
    server_api: Arc<crate::server::server_api::ServerApi>,
    language: Option<String>,
    audio_rx: mpsc::UnboundedReceiver<LiveChunk>,
    audio_paused: Arc<AtomicBool>,
    update_tx: mpsc::UnboundedSender<SocketUpdate>,
) {
    let socket = match server_api.open_transcribe_stream().await {
        Ok(socket) => socket,
        Err(error) => {
            let _ = update_tx.unbounded_send(SocketUpdate::Closed(Some(format!("{error:#}"))));
            return;
        }
    };
    stream_session(socket, language, audio_rx, audio_paused, update_tx).await;
}

/// How many audio chunks pass between progress logs (about ten seconds of audio).
const CHUNKS_PER_LOG: u64 = 1000;

/// Streams audio to the server and its messages back, until the audio ends, the server closes the
/// socket, or either direction fails.
async fn stream_session(
    socket: websocket::WebSocket,
    language: Option<String>,
    mut audio_rx: mpsc::UnboundedReceiver<LiveChunk>,
    audio_paused: Arc<AtomicBool>,
    update_tx: mpsc::UnboundedSender<SocketUpdate>,
) {
    let (mut sink, mut stream) = socket.split().await;
    let config = ClientMessage::Config {
        language,
        vad: Some(VadOptions {
            min_silence_ms: Some(300),
        }),
    };
    let text = |message: &ClientMessage| {
        websocket::Message::new_text(serde_json::to_string(message).expect("messages serialize"))
    };
    if let Err(error) = sink.send(text(&config)).await {
        log::warn!("Hands-free: couldn't send the stream config: {error}");
        let _ = update_tx.unbounded_send(SocketUpdate::Closed(Some(error.to_string())));
        return;
    }
    log::info!("Hands-free: stream config sent, streaming audio");
    let reader_tx = update_tx.clone();
    let reader = async move {
        while let Some(message) = stream.next().await {
            let message = match message {
                Ok(message) => message,
                Err(error) => {
                    log::warn!("Hands-free: socket read failed: {error}");
                    return Some(error.to_string());
                }
            };
            let Some(text) = message.text() else { continue };
            match serde_json::from_str::<ServerMessage>(text) {
                Ok(message) => {
                    log_server_message(&message);
                    let _ = reader_tx.unbounded_send(SocketUpdate::Message(message));
                }
                Err(error) => log::warn!("Unexpected hands-free server message: {error}"),
            }
        }
        log::info!("Hands-free: the server closed the socket");
        None
    };
    let writer = async move {
        let mut was_paused = false;
        let mut sent: u64 = 0;
        let mut dropped: u64 = 0;
        while let Some(chunk) = audio_rx.next().await {
            let paused = audio_paused.load(Ordering::Relaxed);
            if paused {
                // Closing the open utterance stops the server decoding it; the final it sends is
                // dropped because transcripts are paused.
                if !was_paused {
                    log::info!("Hands-free: audio paused because the user is typing");
                    if let Err(error) = sink.send(text(&ClientMessage::Flush)).await {
                        log::warn!("Hands-free: socket write failed: {error}");
                        return Some(error.to_string());
                    }
                }
                was_paused = true;
                dropped += 1;
                continue;
            }
            was_paused = false;
            if sent == 0 {
                log::info!(
                    "Hands-free: sending the first audio chunk ({} bytes)",
                    chunk.pcm.len()
                );
            }
            sent += 1;
            if sent.is_multiple_of(CHUNKS_PER_LOG) {
                log::info!(
                    "Hands-free: sent {sent} audio chunks ({dropped} dropped while paused), \
                     last level {:.3}",
                    chunk.level
                );
            }
            if let Err(error) = sink.send(websocket::Message::new_binary(chunk.pcm)).await {
                log::warn!("Hands-free: socket write failed: {error}");
                return Some(error.to_string());
            }
        }
        log::info!("Hands-free: audio ended after {sent} chunks; ending the stream");
        let _ = sink.send(text(&ClientMessage::End)).await;
        None
    };
    futures::pin_mut!(reader, writer);
    let error = match futures::future::select(reader, writer).await {
        futures::future::Either::Left((error, _)) | futures::future::Either::Right((error, _)) => {
            error
        }
    };
    let _ = update_tx.unbounded_send(SocketUpdate::Closed(error));
}

fn log_server_message(message: &ServerMessage) {
    match message {
        ServerMessage::SpeechStart => log::info!("Hands-free: speech started"),
        ServerMessage::SpeechEnd => log::info!("Hands-free: speech ended"),
        ServerMessage::Partial { utterance, text } => log::info!(
            "Hands-free: partial for utterance {utterance} ({} chars)",
            text.len()
        ),
        ServerMessage::Final { utterance, text } => log::info!(
            "Hands-free: final for utterance {utterance} ({} chars)",
            text.len()
        ),
        ServerMessage::Error { message } => log::warn!("Hands-free: server error: {message}"),
    }
}

impl Entity for HandsFreeSession {
    type Event = HandsFreeEvent;
}

impl SingletonEntity for HandsFreeSession {}

/// Turns hands-free voice input on or off. Turning it on first makes sure the models are
/// installed, asking the user to download them when they aren't.
pub fn toggle_hands_free(window_id: WindowId, ctx: &mut AppContext) {
    if !is_hands_free_enabled() {
        return;
    }
    let session = HandsFreeSession::handle(ctx);
    if session.as_ref(ctx).is_active() {
        session.update(ctx, |session, ctx| session.stop(ctx));
        return;
    }
    if !ensure_ready_to_record(window_id, ctx) {
        return;
    }
    if let Err(message) = session.update(ctx, |session, ctx| session.start(window_id, ctx)) {
        ToastStack::handle(ctx).update(ctx, |stack, ctx| {
            stack.add_ephemeral_toast(DismissibleToast::error(message), window_id, ctx);
        });
    }
}

#[cfg(test)]
#[path = "hands_free_tests.rs"]
mod tests;
