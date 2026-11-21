//! The agent endpoint the client's AI conversation talks to (`POST /ai/multi-agent`). The request
//! is a protobuf `warp_multi_agent_api::Request`; the response is a server-sent event stream whose
//! `data` lines are padded URL-safe base64 protobuf `ResponseEvent`s. See
//! `specs/self-hosted-sso/AGENT_PROTOCOL.md`.

mod prompt;
mod tools;
mod turn;

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE;
use bytes::Bytes;
use futures::StreamExt as _;
use prost::Message as _;
use tokio::sync::mpsc;
use warp_multi_agent_api as api;

use crate::{AppState, bearer_token};

pub async fn multi_agent(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(user) =
        bearer_token(&headers).and_then(|token| state.sessions.user_for_id_token(token))
    else {
        return (StatusCode::UNAUTHORIZED, "sign in required").into_response();
    };
    let request = match api::Request::decode(body) {
        Ok(request) => request,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, format!("invalid request: {error}")).into_response();
        }
    };
    log::info!(
        "Agent turn for {}",
        user.email.as_deref().unwrap_or("<no email>")
    );
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(turn::run(request, tx));
    event_stream(rx)
}

/// Passive suggestions aren't offered; answer with an empty, successful stream.
pub async fn passive_suggestions() -> Response {
    let (tx, rx) = mpsc::unbounded_channel();
    for event in [
        api::response_event::Type::Init(api::response_event::StreamInit {
            conversation_id: uuid::Uuid::new_v4().to_string(),
            request_id: uuid::Uuid::new_v4().to_string(),
            ..Default::default()
        }),
        api::response_event::Type::Finished(api::response_event::StreamFinished {
            reason: Some(api::response_event::stream_finished::Reason::Done(
                Default::default(),
            )),
            ..Default::default()
        }),
    ] {
        let _ = tx.send(api::ResponseEvent {
            r#type: Some(event),
        });
    }
    drop(tx);
    event_stream(rx)
}

fn event_stream(rx: mpsc::UnboundedReceiver<api::ResponseEvent>) -> Response {
    let events = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|event| (event, rx))
    })
    .map(|event| {
        Ok::<_, Infallible>(Event::default().data(URL_SAFE.encode(event.encode_to_vec())))
    });
    Sse::new(events).into_response()
}
