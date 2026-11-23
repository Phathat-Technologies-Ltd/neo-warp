use std::sync::Mutex;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use futures::channel::mpsc;
use local_transcription::stream::VadOptions;
use local_transcription::{Audio, TurnDetector, VAD_FRAME_SAMPLES, VoiceActivityDetector};
use tower::ServiceExt as _;

use super::*;
use crate::oidc::OidcVerifier;
use crate::router;
use crate::sessions::SessionStore;

/// Treats a frame as speech when its first sample is loud.
struct LoudnessVad;

impl VoiceActivityDetector for LoudnessVad {
    fn speech_probability(&mut self, frame: &[f32]) -> Result<f32, TranscribeError> {
        Ok(if frame[0].abs() > 0.25 { 0.9 } else { 0.0 })
    }

    fn reset(&mut self) {}
}

/// Answers with the utterance length in frames, and records the language hints.
#[derive(Default)]
struct FakeTranscriber {
    languages: Mutex<Vec<Option<String>>>,
}

impl Transcriber for FakeTranscriber {
    fn transcribe(&self, audio: &Audio, language: Option<&str>) -> Result<String, TranscribeError> {
        self.languages
            .lock()
            .unwrap()
            .push(language.map(str::to_owned));
        Ok(format!(
            "{} frames",
            audio.samples.len() / VAD_FRAME_SAMPLES
        ))
    }

    fn is_ready(&self) -> bool {
        true
    }
}

fn vad_factory() -> VadFactory {
    Arc::new(|| Ok(Box::new(LoudnessVad) as Box<dyn VoiceActivityDetector>))
}

fn pcm(amplitude: f32, frames: usize) -> Frame {
    let sample = (amplitude * 32767.0) as i16;
    Frame::Binary(
        std::iter::repeat_n(sample.to_le_bytes(), frames * VAD_FRAME_SAMPLES)
            .flatten()
            .collect(),
    )
}

fn text(message: &ClientMessage) -> Frame {
    Frame::Text(serde_json::to_string(message).unwrap())
}

/// Runs a session over `frames` and returns what the server sent.
async fn run(frames: Vec<Frame>, transcriber: Arc<FakeTranscriber>) -> Vec<Frame> {
    run_with_turn_detector(frames, transcriber, None).await
}

async fn run_with_turn_detector(
    frames: Vec<Frame>,
    transcriber: Arc<FakeTranscriber>,
    turn_detector: Option<TurnDetectorFactory>,
) -> Vec<Frame> {
    let (tx, rx) = mpsc::unbounded();
    let incoming = futures::stream::iter(frames).chain(futures::stream::pending());
    tokio::time::timeout(
        Duration::from_secs(10),
        run_session(
            Box::pin(incoming),
            tx,
            transcriber,
            vad_factory(),
            turn_detector,
        ),
    )
    .await
    .expect("the session ends after `end`");
    rx.collect().await
}

fn server_messages(frames: &[Frame]) -> Vec<ServerMessage> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            Frame::Text(text) => Some(serde_json::from_str(text).unwrap()),
            Frame::Binary(_) | Frame::Close => None,
        })
        .collect()
}

#[tokio::test]
async fn streams_partials_and_finals() {
    let transcriber = Arc::new(FakeTranscriber::default());
    let sent = run(
        vec![
            text(&ClientMessage::Config {
                language: Some("en".to_owned()),
                vad: None,
            }),
            pcm(0.0, 5),
            pcm(0.5, 30),
            pcm(0.0, 12),
            text(&ClientMessage::End),
        ],
        transcriber.clone(),
    )
    .await;
    assert_eq!(sent.last(), Some(&Frame::Close));
    let messages = server_messages(&sent);
    assert_eq!(messages.first(), Some(&ServerMessage::SpeechStart));
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, ServerMessage::Partial { utterance: 1, .. })),
        "{messages:?}"
    );
    assert!(messages.contains(&ServerMessage::SpeechEnd));
    let finals: Vec<_> = messages
        .iter()
        .filter(|m| matches!(m, ServerMessage::Final { .. }))
        .collect();
    assert_eq!(finals.len(), 1, "{messages:?}");
    assert!(
        transcriber
            .languages
            .lock()
            .unwrap()
            .iter()
            .all(|language| language.as_deref() == Some("en"))
    );
}

#[tokio::test]
async fn end_flushes_the_open_utterance() {
    let sent = run(
        vec![pcm(0.5, 5), text(&ClientMessage::End)],
        Arc::new(FakeTranscriber::default()),
    )
    .await;
    let messages = server_messages(&sent);
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, ServerMessage::Final { utterance: 1, .. })),
        "{messages:?}"
    );
}

#[tokio::test]
async fn reports_invalid_messages_and_keeps_going() {
    let sent = run(
        vec![
            Frame::Text("{\"type\":\"bogus\"}".to_owned()),
            text(&ClientMessage::End),
        ],
        Arc::new(FakeTranscriber::default()),
    )
    .await;
    assert!(matches!(
        server_messages(&sent).as_slice(),
        [ServerMessage::Error { .. }]
    ));
}

#[tokio::test]
async fn reports_a_missing_vad_model() {
    let (tx, rx) = mpsc::unbounded();
    let vad: VadFactory = Arc::new(|| Err(TranscribeError::ModelNotInstalled));
    run_session(
        Box::pin(futures::stream::pending()),
        tx,
        Arc::new(FakeTranscriber::default()),
        vad,
        None,
    )
    .await;
    let sent: Vec<_> = rx.collect().await;
    assert!(matches!(
        server_messages(&sent).as_slice(),
        [ServerMessage::Error { .. }]
    ));
}

fn state(enabled: bool) -> Arc<AppState> {
    Arc::new(AppState {
        verifier: OidcVerifier::new("https://issuer.example", "client"),
        sessions: SessionStore::new(Some(b"test-secret"), Duration::from_secs(3600)),
        required_group: None,
        remote_server_artifacts: None,
        transcriber: enabled.then(|| Arc::new(FakeTranscriber::default()) as Arc<dyn Transcriber>),
        vad: enabled.then(vad_factory),
        turn_detector: None,
        ai_settings: Default::default(),
        make_provider: crate::agent::default_provider_factory(),
        codebase_index: None,
        recordings: None,
    })
}

async fn upgrade_status(state: Arc<AppState>, token: Option<&str>) -> StatusCode {
    let mut request = Request::get("/ai/transcribe/stream")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    router(state)
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn requires_sign_in() {
    assert_eq!(
        upgrade_status(state(true), None).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn reports_when_hands_free_is_off() {
    let state = state(false);
    let custom = state
        .sessions
        .sign_in("https://issuer.example", "subject", None, None);
    let token = state
        .sessions
        .redeem_custom_token(&custom)
        .await
        .unwrap()
        .id_token;
    assert_eq!(
        upgrade_status(state, Some(&token)).await,
        StatusCode::NOT_IMPLEMENTED
    );
}

/// Always judges the turn finished.
struct CompleteTurnDetector;

impl TurnDetector for CompleteTurnDetector {
    fn completion_probability(&mut self, _audio: &[f32]) -> Result<f32, TranscribeError> {
        Ok(1.0)
    }
}

fn complete_turn_detector() -> TurnDetectorFactory {
    Arc::new(|| Ok(Box::new(CompleteTurnDetector) as Box<dyn TurnDetector>))
}

/// Whether 8 frames (256 ms) of silence after speech end the utterance: past the 200 ms probe,
/// short of the 300 ms fixed timeout.
async fn short_pause_ends_utterance(
    config: Option<ClientMessage>,
    turn_detector: Option<TurnDetectorFactory>,
) -> bool {
    let mut frames: Vec<Frame> = config.iter().map(text).collect();
    frames.extend([pcm(0.5, 10), pcm(0.0, 8), pcm(0.5, 10)]);
    frames.push(text(&ClientMessage::End));
    let sent =
        run_with_turn_detector(frames, Arc::new(FakeTranscriber::default()), turn_detector).await;
    let finals = server_messages(&sent)
        .iter()
        .filter(|message| matches!(message, ServerMessage::Final { .. }))
        .count();
    finals == 2
}

#[tokio::test]
async fn turn_detector_ends_finished_turns_early() {
    assert!(short_pause_ends_utterance(None, Some(complete_turn_detector())).await);
}

#[tokio::test]
async fn smart_end_of_turn_can_be_turned_off() {
    let config = ClientMessage::Config {
        language: None,
        vad: Some(VadOptions {
            min_silence_ms: Some(300),
            smart_end_of_turn: Some(false),
        }),
    };
    assert!(!short_pause_ends_utterance(Some(config), Some(complete_turn_detector())).await);
}

#[tokio::test]
async fn missing_turn_model_falls_back_to_the_silence_timeout() {
    let factory: TurnDetectorFactory = Arc::new(|| Err(TranscribeError::ModelNotInstalled));
    assert!(!short_pause_ends_utterance(None, Some(factory)).await);
}
