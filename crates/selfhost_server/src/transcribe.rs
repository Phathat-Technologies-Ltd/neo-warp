//! `/ai/transcribe`: Warp's voice transcription endpoint, answered by a local
//! [`local_transcription::Transcriber`] instead of a hosted provider. The request and response
//! bodies match Warp's (`app/src/ai/voice/transcribe/api`); provider-specific fields are ignored.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use local_transcription::{TranscribeError, decode_wav};
use serde::{Deserialize, Serialize};

use crate::{AppState, bearer_token};

#[derive(Deserialize)]
pub(crate) struct TranscribeRequest {
    /// Base64-encoded WAV.
    audio: Option<String>,
    language: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct TranscribeResponse {
    text: String,
}

pub(crate) async fn transcribe(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<TranscribeRequest>,
) -> Response {
    if bearer_token(&headers)
        .and_then(|token| state.sessions.user_for_id_token(token))
        .is_none()
    {
        return (StatusCode::UNAUTHORIZED, "sign in required").into_response();
    }
    let Some(transcriber) = state.transcriber.clone() else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "voice transcription isn't enabled",
        )
            .into_response();
    };
    let Some(audio) = request.audio else {
        return (StatusCode::BAD_REQUEST, "audio is required").into_response();
    };
    let wav = match base64::engine::general_purpose::STANDARD.decode(audio) {
        Ok(wav) => wav,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, format!("invalid audio: {error}")).into_response();
        }
    };
    let language = request.language;
    let result = tokio::task::spawn_blocking(move || {
        let audio = decode_wav(&wav)?;
        transcriber.transcribe(&audio, language.as_deref())
    })
    .await;
    match result {
        Ok(Ok(text)) => Json(TranscribeResponse { text }).into_response(),
        Ok(Err(error @ TranscribeError::ModelNotInstalled)) => {
            (StatusCode::SERVICE_UNAVAILABLE, error.to_string()).into_response()
        }
        Ok(Err(error @ TranscribeError::InvalidAudio(_))) => {
            (StatusCode::BAD_REQUEST, error.to_string()).into_response()
        }
        Ok(Err(error @ TranscribeError::Engine(_))) => {
            log::error!("Transcription failed: {error}");
            (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
        }
        Err(error) => {
            log::error!("Transcription task failed: {error}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
#[path = "transcribe_tests.rs"]
mod tests;
