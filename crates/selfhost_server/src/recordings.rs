//! `/recordings`: storage for terminal session recordings. Clients capture and redact
//! recordings themselves and stream already-redacted asciicast events here; each signed-in user
//! only sees their own recordings. The backend is whatever [`RecordingStore`] the server was
//! configured with.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use session_recording::{
    AppendRequest, ByteRange, CreateRecording, CreatedRecording, RecordingStore, RenameRecording,
    StoreError,
};

use crate::{AppState, bearer_token};

const CAST_CONTENT_TYPE: &str = "application/x-asciicast";

type Owner = String;

/// The signed-in user and the store, or the response to send instead.
#[allow(clippy::result_large_err)]
fn authorize(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(Owner, Arc<dyn RecordingStore>), Response> {
    let Some(user) =
        bearer_token(headers).and_then(|token| state.sessions.user_for_id_token(token))
    else {
        return Err((StatusCode::UNAUTHORIZED, "sign in required").into_response());
    };
    let Some(store) = state.recordings.clone() else {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "session recording storage isn't enabled",
        )
            .into_response());
    };
    Ok((user.uid, store))
}

/// Runs a blocking store operation off the async workers.
#[allow(clippy::result_large_err)]
async fn run<T: Send + 'static>(
    store: Arc<dyn RecordingStore>,
    op: impl FnOnce(&dyn RecordingStore) -> Result<T, StoreError> + Send + 'static,
) -> Result<T, Response> {
    match tokio::task::spawn_blocking(move || op(store.as_ref())).await {
        Ok(result) => result.map_err(error_response),
        Err(error) => {
            log::error!("Recording store task failed: {error}");
            Err(StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

fn error_response(error: StoreError) -> Response {
    match error {
        StoreError::NotFound => (StatusCode::NOT_FOUND, error.to_string()).into_response(),
        StoreError::AlreadyExists(_) | StoreError::InProgress => {
            (StatusCode::CONFLICT, error.to_string()).into_response()
        }
        StoreError::OutOfOrder { expected } => (
            StatusCode::CONFLICT,
            Json(json!({ "error": error.to_string(), "expected_seq": expected })),
        )
            .into_response(),
        StoreError::RangeNotSatisfiable { total_len } => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{total_len}"))],
        )
            .into_response(),
        StoreError::Invalid(message) => (StatusCode::BAD_REQUEST, message).into_response(),
        StoreError::Io(error) => {
            log::error!("Recording store I/O failed: {error}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "recording storage failed",
            )
                .into_response()
        }
    }
}

fn respond<T>(result: Result<T, Response>, ok: impl FnOnce(T) -> Response) -> Response {
    match result {
        Ok(value) => ok(value),
        Err(response) => response,
    }
}

pub(crate) async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateRecording>,
) -> Response {
    let (owner, store) = match authorize(&state, &headers) {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    let result = run(store, move |store| store.create(&owner, &request)).await;
    respond(result, |id| {
        (StatusCode::CREATED, Json(CreatedRecording { id })).into_response()
    })
}

pub(crate) async fn append(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<AppendRequest>,
    events: String,
) -> Response {
    let (owner, store) = match authorize(&state, &headers) {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    let result = run(store, move |store| {
        store.append(&owner, &id, query.seq, &events)
    })
    .await;
    respond(result, |()| StatusCode::NO_CONTENT.into_response())
}

pub(crate) async fn finish(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let (owner, store) = match authorize(&state, &headers) {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    let result = run(store, move |store| store.finish(&owner, &id)).await;
    respond(result, |metadata| Json(metadata).into_response())
}

pub(crate) async fn list(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let (owner, store) = match authorize(&state, &headers) {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    let result = run(store, move |store| store.list(&owner)).await;
    respond(result, |recordings| Json(recordings).into_response())
}

/// The recording's asciicast content. Honors a single `Range: bytes=` range so long recordings
/// can be fetched in pieces.
pub(crate) async fn read(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let (owner, store) = match authorize(&state, &headers) {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    let range = match headers.get(header::RANGE) {
        Some(value) => match value.to_str().ok().and_then(ByteRange::parse_header) {
            Some(range) => Some(range),
            None => return (StatusCode::BAD_REQUEST, "unsupported range").into_response(),
        },
        None => None,
    };
    let result = run(store, move |store| store.read(&owner, &id, range)).await;
    respond(result, |content| {
        let end = content.start + content.bytes.len() as u64;
        let mut response = content.bytes.into_response();
        let response_headers = response.headers_mut();
        response_headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(CAST_CONTENT_TYPE),
        );
        response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        if range.is_some() {
            let value = format!(
                "bytes {}-{}/{}",
                content.start,
                end.saturating_sub(1),
                content.total_len
            );
            if let Ok(value) = HeaderValue::from_str(&value) {
                response_headers.insert(header::CONTENT_RANGE, value);
            }
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        }
        response
    })
}

pub(crate) async fn rename(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<RenameRecording>,
) -> Response {
    let (owner, store) = match authorize(&state, &headers) {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    let result = run(store, move |store| store.rename(&owner, &id, &request.name)).await;
    respond(result, |metadata| Json(metadata).into_response())
}

pub(crate) async fn delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let (owner, store) = match authorize(&state, &headers) {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    let result = run(store, move |store| store.delete(&owner, &id)).await;
    respond(result, |()| StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
#[path = "recordings_tests.rs"]
mod tests;
