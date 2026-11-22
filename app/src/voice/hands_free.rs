//! Hands-free voice input: the microphone stays open, the embedded server splits speech into
//! utterances (`/ai/transcribe/stream`), and transcripts flow into the focused input. Audio stays
//! on the device and is never written to disk.

use instant::Instant;
use std::sync::Arc;

use futures::channel::mpsc;
use futures::{SinkExt as _, StreamExt as _};
use local_transcription::stream::{ClientMessage, ServerMessage, VadOptions};
use voice_input::{LiveCapture, LiveChunk};
use warpui::r#async::Timer;
use warpui::platform::MicrophoneAccessState;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity, WindowId};
use websocket::WebsocketMessage as _;

use super::hands_free_controller::{
    HandsFreeController, HandsFreeEffect, HandsFreeOptions, HandsFreeState,
};
use super::local_model::{ensure_ready_to_record, is_hands_free_enabled};
use crate::server::server_api::ServerApiProvider;
use crate::settings::AISettings;
use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;

pub enum HandsFreeEvent {
    StateChanged,
    /// An edit for the focused input.
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
    /// Whether finals are rewritten as terminal syntax, which suits shell commands but not agent
    /// prompts.
    normalize_finals: bool,
}

impl HandsFreeSession {
    pub fn state(&self) -> HandsFreeState {
        self.controller.state()
    }

    pub fn is_active(&self) -> bool {
        self.controller.state().is_mic_on()
    }

    pub fn user_typed(&mut self, ctx: &mut ModelContext<Self>) {
        self.controller.user_typed();
        ctx.notify();
    }

    pub fn input_reset(&mut self, ctx: &mut ModelContext<Self>) {
        self.controller.input_reset();
        ctx.notify();
    }

    /// Delivers a server message to the controller, rewriting finals as terminal syntax when
    /// `normalize` is set.
    fn handle_message(&mut self, message: ServerMessage, ctx: &mut ModelContext<Self>) {
        let normalize = self.normalize_finals;
        let before = self.controller.state();
        let effects = self
            .controller
            .on_message(message, normalize, Instant::now());
        for effect in effects {
            ctx.emit(HandsFreeEvent::Effect(effect));
        }
        self.schedule_auto_send(ctx);
        if self.controller.state() != before {
            ctx.emit(HandsFreeEvent::StateChanged);
        }
        ctx.notify();
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

    fn start(&mut self, ctx: &mut ModelContext<Self>) -> Result<(), String> {
        if matches!(
            ctx.microphone_access_state(),
            MicrophoneAccessState::Denied | MicrophoneAccessState::Restricted
        ) {
            return Err("Microphone access is denied".to_owned());
        }
        let (audio_tx, audio_rx) = mpsc::unbounded::<LiveChunk>();
        let capture = LiveCapture::start(move |chunk| {
            let _ = audio_tx.unbounded_send(chunk);
        })
        .map_err(|error| format!("Couldn't open the microphone: {error:#}"))?;
        self.generation += 1;
        self.capture = Some(capture);
        let settings = AISettings::as_ref(ctx);
        let options = HandsFreeOptions {
            auto_send_after: settings.hands_free_auto_send_after(),
            voice_commands: *settings.hands_free_voice_commands,
        };
        let language = settings.voice_input_language_code().map(str::to_owned);
        self.controller.start(options);

        let (update_tx, update_rx) = mpsc::unbounded();
        let server_api = ServerApiProvider::as_ref(ctx).get();
        let generation = self.generation;
        ctx.spawn(
            run_socket(server_api, language, audio_rx, update_tx),
            |_, _, _| {},
        );
        ctx.spawn_stream_local(
            update_rx,
            move |me, update, ctx| {
                if me.generation != generation {
                    return;
                }
                match update {
                    SocketUpdate::Message(message) => me.handle_message(message, ctx),
                    SocketUpdate::Closed(error) => {
                        if let Some(error) = error {
                            log::warn!("Hands-free voice input stopped: {error}");
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
        if self.controller.state().is_mic_on() {
            self.controller.stop();
            ctx.emit(HandsFreeEvent::StateChanged);
            ctx.notify();
        }
    }

    /// Whether finals are rewritten as terminal syntax; set by the input that receives them.
    pub fn set_normalize_finals(&mut self, normalize: bool) {
        self.normalize_finals = normalize;
    }
}

/// Streams audio to the server and its messages back, until the audio ends or the socket fails.
async fn run_socket(
    server_api: Arc<crate::server::server_api::ServerApi>,
    language: Option<String>,
    mut audio_rx: mpsc::UnboundedReceiver<LiveChunk>,
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
            let Ok(message) = message else { break };
            let Some(text) = message.text() else { continue };
            match serde_json::from_str::<ServerMessage>(text) {
                Ok(message) => {
                    let _ = reader_tx.unbounded_send(SocketUpdate::Message(message));
                }
                Err(error) => log::warn!("Unexpected hands-free server message: {error}"),
            }
        }
    };
    let writer = async move {
        while let Some(chunk) = audio_rx.next().await {
            if sink
                .send(websocket::Message::new_binary(chunk.pcm))
                .await
                .is_err()
            {
                return update_tx;
            }
        }
        let _ = sink.send(text(&ClientMessage::End)).await;
        update_tx
    };
    let (_, update_tx) = futures::join!(reader, writer);
    let _ = update_tx.unbounded_send(SocketUpdate::Closed(None));
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
    if let Err(message) = session.update(ctx, |session, ctx| session.start(ctx)) {
        ToastStack::handle(ctx).update(ctx, |stack, ctx| {
            stack.add_ephemeral_toast(DismissibleToast::error(message), window_id, ctx);
        });
    }
}
