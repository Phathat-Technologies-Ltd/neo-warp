//! The agent endpoint the client's AI conversation talks to (`POST /ai/multi-agent`). The request
//! is a protobuf `warp_multi_agent_api::Request`; the response is a server-sent event stream whose
//! `data` lines are padded URL-safe base64 protobuf `ResponseEvent`s. See
//! `specs/self-hosted-sso/AGENT_PROTOCOL.md`.

pub mod command_search;
mod helpers;
mod mcp;
pub mod next_command;
mod monitor;
mod prompt;
mod responses;
#[cfg(test)]
pub(crate) mod scripted;
pub mod suggestions;
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

pub use helpers::ModelSettingsCache;
pub use next_command::NextCommandState;
pub use turn::{ModelEndpoint, ProviderFactory};

use crate::{AppState, bearer_token};

/// Builds providers for the user's real model endpoints.
pub fn default_provider_factory() -> ProviderFactory {
    Arc::new(turn::provider_for)
}

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
    state
        .ai_settings
        .record(&user.uid, request.settings.as_ref());
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(turn::run_with_provider(
        request,
        tx,
        state.make_provider.clone(),
    ));
    event_stream(rx)
}

/// `POST /ai/passive-suggestions`: a prompt suggestion from the user's model, if it has one.
pub async fn passive_suggestions(
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
    state
        .ai_settings
        .record(&user.uid, request.settings.as_ref());
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(suggestions::suggest(
        request,
        tx,
        state.make_provider.clone(),
    ));
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
