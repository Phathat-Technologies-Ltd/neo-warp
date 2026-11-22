use std::io::Cursor;
use std::sync::Mutex;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, header};
use local_transcription::{Audio, Transcriber};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::*;
use crate::oidc::OidcVerifier;
use crate::router;
use crate::sessions::SessionStore;

/// Records what it was asked and answers with fixed text, or with "model not installed".
struct FakeTranscriber {
    ready: bool,
    calls: Mutex<Vec<(Audio, Option<String>)>>,
}

impl Transcriber for FakeTranscriber {
    fn transcribe(&self, audio: &Audio, language: Option<&str>) -> Result<String, TranscribeError> {
        if !self.ready {
            return Err(TranscribeError::ModelNotInstalled);
        }
        self.calls
            .lock()
            .unwrap()
            .push((audio.clone(), language.map(str::to_owned)));
        Ok("hello world".to_owned())
    }

    fn is_ready(&self) -> bool {
        self.ready
    }
}

fn state(transcriber: Option<Arc<FakeTranscriber>>) -> Arc<AppState> {
    Arc::new(AppState {
        verifier: OidcVerifier::new("https://issuer.example", "client"),
        sessions: SessionStore::new(Some(b"test-secret"), Duration::from_secs(3600)),
        required_group: None,
        remote_server_artifacts: None,
        transcriber: transcriber.map(|t| t as Arc<dyn Transcriber>),
        vad: None,
        ai_settings: Default::default(),
        make_provider: crate::agent::default_provider_factory(),
        codebase_index: None,
    })
}

async fn id_token(state: &AppState) -> String {
    let custom = state
        .sessions
        .sign_in("https://issuer.example", "subject", None, None);
    state
        .sessions
        .redeem_custom_token(&custom)
        .await
        .unwrap()
        .id_token
}

fn wav_base64() -> String {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
    for _ in 0..160 {
        writer.write_sample(0_i16).unwrap();
    }
    writer.finalize().unwrap();
    base64::engine::general_purpose::STANDARD.encode(cursor.into_inner())
}

async fn post(state: &Arc<AppState>, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut request =
        Request::post("/ai/transcribe").header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = router(state.clone())
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The body the client sends (`ServerVoiceTranscriber`), provider field included.
fn client_body() -> Value {
    json!({ "provider": "wispr", "audio": wav_base64(), "language": "en" })
}

#[tokio::test]
async fn transcribes_the_client_request() {
    let transcriber = Arc::new(FakeTranscriber {
        ready: true,
        calls: Mutex::default(),
    });
    let state = state(Some(transcriber.clone()));
    let token = id_token(&state).await;
    let (status, body) = post(&state, Some(&token), client_body()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "text": "hello world" }));
    let calls = transcriber.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0.sample_rate, 16000);
    assert_eq!(calls[0].0.samples.len(), 160);
    assert_eq!(calls[0].1.as_deref(), Some("en"));
}

#[tokio::test]
async fn requires_sign_in() {
    let state = state(None);
    let (status, _) = post(&state, None, client_body()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn reports_disabled_missing_model_and_bad_audio() {
    let disabled = state(None);
    let token = id_token(&disabled).await;
    assert_eq!(
        post(&disabled, Some(&token), client_body()).await.0,
        StatusCode::NOT_IMPLEMENTED
    );

    let missing = state(Some(Arc::new(FakeTranscriber {
        ready: false,
        calls: Mutex::default(),
    })));
    let token = id_token(&missing).await;
    assert_eq!(
        post(&missing, Some(&token), client_body()).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );

    let ready = state(Some(Arc::new(FakeTranscriber {
        ready: true,
        calls: Mutex::default(),
    })));
    let token = id_token(&ready).await;
    assert_eq!(
        post(&ready, Some(&token), json!({ "audio": "bm90IGEgd2F2" }))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}
