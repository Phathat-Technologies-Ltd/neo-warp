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
use warpui::{AppContext, Entity, ModelContext, SingletonEntity, WindowId};
use websocket::WebsocketMessage as _;

use super::conversation::ConversationSession;
use super::hands_free_controller::{
    HandsFreeController, HandsFreeEffect, HandsFreeOptions, HandsFreeState,
};
use super::local_model::{ensure_ready_to_record, is_hands_free_enabled};
use super::speaker_gate::{AudioTap, GateStep, SpeakerGate};
use super::speaker_id::SpeakerProfile;
use crate::server::server_api::ServerApiProvider;
use crate::settings::AISettings;
use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;

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
}

/// How long "Not you — ignored" shows.
const REJECTION_NOTICE: Duration = Duration::from_secs(3);

impl HandsFreeSession {
    pub fn state(&self) -> HandsFreeState {
        self.controller.state()
    }

    pub fn is_active(&self) -> bool {
        self.controller.state().is_mic_on()
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
        let now = self.tap_total();
        let verify = self.verify_speaker && !self.controller.is_paused();
        let steps = self.gate.push(message, now, verify);
        self.run_gate_steps(steps, ctx);
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
                        let verify = me.verify_speaker && !me.controller.is_paused();
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
        let effects = self.controller.on_message(message, Instant::now());
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
            message @ (ServerMessage::Partial { .. } | ServerMessage::Error { .. }) => message,
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
        self.window_id = Some(window_id);
        self.audio_paused = Arc::new(AtomicBool::new(false));
        self.verify_speaker = verify_speaker;
        self.sensitivity = sensitivity;
        self.gate = SpeakerGate::default();
        self.tap = tap;
        self.rejected_until = None;
        let settings = AISettings::as_ref(ctx);
        let options = HandsFreeOptions {
            auto_send_after: settings.hands_free_auto_send_after(),
            voice_commands: *settings.hands_free_voice_commands,
        };
        let language = settings.voice_input_language_code().map(str::to_owned);
        self.controller.start(options);

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
        ctx.emit(HandsFreeEvent::StateChanged);
        ctx.notify();
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
        if self.controller.state().is_mic_on() {
            self.controller.stop();
            ctx.emit(HandsFreeEvent::StateChanged);
            ctx.notify();
        }
    }
}

/// Streams audio to the server and its messages back, until the audio ends, the server closes the
/// socket, or either direction fails.
async fn run_socket(
    server_api: Arc<crate::server::server_api::ServerApi>,
    language: Option<String>,
    mut audio_rx: mpsc::UnboundedReceiver<LiveChunk>,
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
        let _ = update_tx.unbounded_send(SocketUpdate::Closed(Some(error.to_string())));
        return;
    }
    let reader_tx = update_tx.clone();
    let reader = async move {
        while let Some(message) = stream.next().await {
            let message = match message {
                Ok(message) => message,
                Err(error) => return Some(error.to_string()),
            };
            let Some(text) = message.text() else { continue };
            match serde_json::from_str::<ServerMessage>(text) {
                Ok(message) => {
                    let _ = reader_tx.unbounded_send(SocketUpdate::Message(message));
                }
                Err(error) => log::warn!("Unexpected hands-free server message: {error}"),
            }
        }
        None
    };
    let writer = async move {
        let mut was_paused = false;
        while let Some(chunk) = audio_rx.next().await {
            let paused = audio_paused.load(Ordering::Relaxed);
            if paused {
                // Closing the open utterance stops the server decoding it; the final it sends is
                // dropped because transcripts are paused.
                if !was_paused && let Err(error) = sink.send(text(&ClientMessage::Flush)).await {
                    return Some(error.to_string());
                }
                was_paused = true;
                continue;
            }
            was_paused = false;
            if let Err(error) = sink.send(websocket::Message::new_binary(chunk.pcm)).await {
                return Some(error.to_string());
            }
        }
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
