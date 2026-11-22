//! `/ai/transcribe/stream`: hands-free voice input over a WebSocket. The client streams 16 kHz
//! mono PCM; the server splits it into utterances with a voice activity detector and answers with
//! live partial and final transcripts. The protocol types live in
//! [`local_transcription::stream`].

use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::{Sink, SinkExt as _, Stream, StreamExt as _};
use local_transcription::stream::{
    ClientMessage, DecodeQueue, DecodeRequest, ServerMessage, StreamConfig, StreamOutput,
    UtteranceStream,
};
use local_transcription::{TranscribeError, Transcriber, VadFactory};

use crate::{AppState, bearer_token};

pub(crate) async fn transcribe_stream(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if bearer_token(&headers)
        .and_then(|token| state.sessions.user_for_id_token(token))
        .is_none()
    {
        return (StatusCode::UNAUTHORIZED, "sign in required").into_response();
    }
    let (Some(transcriber), Some(vad)) = (state.transcriber.clone(), state.vad.clone()) else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "hands-free voice input isn't enabled",
        )
            .into_response();
    };
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    upgrade
        .on_upgrade(move |socket| async move {
            let (sink, stream) = socket.split();
            let stream = stream.filter_map(|message| async move { frame_from_ws(message) });
            let sink = sink.with(|frame: Frame| async move {
                Ok::<_, axum::Error>(match frame {
                    Frame::Text(text) => Message::Text(text.into()),
                    Frame::Binary(bytes) => Message::Binary(bytes.into()),
                    Frame::Close => Message::Close(None),
                })
            });
            run_session(Box::pin(stream), Box::pin(sink), transcriber, vad).await;
        })
        .into_response()
}

fn frame_from_ws(message: Result<Message, axum::Error>) -> Option<Frame> {
    match message {
        Ok(Message::Text(text)) => Some(Frame::Text(text.to_string())),
        Ok(Message::Binary(bytes)) => Some(Frame::Binary(bytes.to_vec())),
        Ok(Message::Close(_)) | Err(_) => Some(Frame::Close),
        Ok(Message::Ping(_) | Message::Pong(_)) => None,
    }
}

/// A WebSocket frame, independent of the WebSocket implementation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Frame {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

/// Audio frames between progress logs.
const AUDIO_FRAMES_PER_LOG: u64 = 1000;

type DecodeResult = (DecodeRequest, Result<String, TranscribeError>);

/// Runs one streaming session until the client ends or closes it. At most one decode runs at a
/// time; requests that arrive meanwhile wait in a [`DecodeQueue`].
pub(crate) async fn run_session<In, Out>(
    mut incoming: In,
    mut outgoing: Out,
    transcriber: Arc<dyn Transcriber>,
    vad: VadFactory,
) where
    In: Stream<Item = Frame> + Unpin,
    Out: Sink<Frame> + Unpin,
{
    let detector = match tokio::task::spawn_blocking(move || vad()).await {
        Ok(Ok(detector)) => detector,
        Ok(Err(error)) => {
            log::warn!("Hands-free stream: couldn't load the voice activity detector: {error}");
            let _ = send(&mut outgoing, &error_message(&error)).await;
            let _ = outgoing.send(Frame::Close).await;
            return;
        }
        Err(error) => {
            log::error!("Couldn't load the voice activity detector: {error}");
            let _ = outgoing.send(Frame::Close).await;
            return;
        }
    };
    log::info!("Hands-free stream: session started, voice activity detector loaded");
    let mut utterances = UtteranceStream::new(detector, StreamConfig::default());
    let mut audio_frames: u64 = 0;
    let mut audio_bytes: u64 = 0;
    let mut queue = DecodeQueue::default();
    let mut running: Option<tokio::task::JoinHandle<DecodeResult>> = None;
    let mut language: Option<String> = None;
    let mut ending = false;
    loop {
        if running.is_none() {
            if let Some(request) = queue.pop() {
                let transcriber = transcriber.clone();
                let language = language.clone();
                log::info!(
                    "Hands-free stream: decoding {:?} of utterance {} ({} ms of audio)",
                    request.kind,
                    request.utterance,
                    request.audio.samples.len() as u64 * 1000
                        / u64::from(request.audio.sample_rate.max(1))
                );
                running = Some(tokio::task::spawn_blocking(move || {
                    let result = transcriber.transcribe(&request.audio, language.as_deref());
                    (request, result)
                }));
            } else if ending {
                break;
            }
        }
        let mut outputs = Vec::new();
        tokio::select! {
            frame = incoming.next(), if !ending => match frame {
                None | Some(Frame::Close) => {
                    log::info!(
                        "Hands-free stream: client closed after {audio_frames} audio frames \
                         ({audio_bytes} bytes)"
                    );
                    return;
                }
                Some(Frame::Binary(bytes)) => {
                    audio_frames += 1;
                    audio_bytes += bytes.len() as u64;
                    if audio_frames == 1 {
                        log::info!(
                            "Hands-free stream: first audio frame ({} bytes)",
                            bytes.len()
                        );
                    } else if audio_frames.is_multiple_of(AUDIO_FRAMES_PER_LOG) {
                        log::info!(
                            "Hands-free stream: {audio_frames} audio frames ({audio_bytes} bytes), \
                             in speech: {}",
                            utterances.in_speech()
                        );
                    }
                    match utterances.push_pcm(&bytes) {
                    Ok(more) => outputs = more,
                    Err(error) => {
                        let _ = send(&mut outgoing, &error_message(&error)).await;
                        return;
                    }
                }},
                Some(Frame::Text(text)) => match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(ClientMessage::Config { language: new_language, vad }) => {
                        if new_language.is_some() {
                            language = new_language;
                        }
                        if let Some(min_silence_ms) = vad.and_then(|vad| vad.min_silence_ms) {
                            utterances.set_min_silence_ms(min_silence_ms.clamp(100, 5000));
                        }
                    }
                    Ok(ClientMessage::Flush) => outputs = utterances.flush(),
                    Ok(ClientMessage::End) => {
                        outputs = utterances.flush();
                        ending = true;
                    }
                    Err(error) => {
                        let message = ServerMessage::Error {
                            message: format!("invalid message: {error}"),
                        };
                        if send(&mut outgoing, &message).await.is_err() {
                            return;
                        }
                    }
                },
            },
            result = async { running.as_mut().expect("guarded by the select condition").await },
                if running.is_some() =>
            {
                running = None;
                let message = match result {
                    Ok((request, Ok(text))) => {
                        log::info!(
                            "Hands-free stream: decoded {:?} of utterance {} ({} chars)",
                            request.kind,
                            request.utterance,
                            text.len()
                        );
                        request.into_message(text)
                    }
                    Ok((_, Err(error))) => error_message(&error),
                    Err(error) => {
                        log::error!("Streaming transcription task failed: {error}");
                        ServerMessage::Error {
                            message: "transcription failed".to_owned(),
                        }
                    }
                };
                if send(&mut outgoing, &message).await.is_err() {
                    return;
                }
            }
        }
        for output in outputs {
            match output {
                StreamOutput::Message(message) => {
                    match &message {
                        ServerMessage::SpeechStart => log::info!("Hands-free stream: speech start"),
                        ServerMessage::SpeechEnd => log::info!("Hands-free stream: speech end"),
                        ServerMessage::Partial { .. }
                        | ServerMessage::Final { .. }
                        | ServerMessage::Error { .. } => {}
                    }
                    if send(&mut outgoing, &message).await.is_err() {
                        return;
                    }
                }
                StreamOutput::Decode(request) => queue.push(request),
            }
        }
    }
    let _ = outgoing.send(Frame::Close).await;
}

fn error_message(error: &TranscribeError) -> ServerMessage {
    if let TranscribeError::Engine(_) = error {
        log::error!("Streaming transcription failed: {error}");
    }
    ServerMessage::Error {
        message: error.to_string(),
    }
}

async fn send<Out: Sink<Frame> + Unpin>(
    outgoing: &mut Out,
    message: &ServerMessage,
) -> Result<(), Out::Error> {
    let text = serde_json::to_string(message).expect("server messages serialize");
    outgoing.send(Frame::Text(text)).await
}

#[cfg(test)]
#[path = "transcribe_stream_tests.rs"]
mod tests;
