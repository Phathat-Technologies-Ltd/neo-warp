//! A self-hosted replacement for the parts of Warp's server that sign users in: it trades an
//! organization identity provider's ID token for the client's account tokens and answers the
//! client's user query. It runs standalone (`selfhost-server`) or embedded in the app
//! ([`spawn_embedded`]).

pub mod agent;
pub mod config;
pub mod graphql;
pub mod oidc;
pub mod sessions;

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;

use axum::extract::{DefaultBodyLimit, Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::config::ServerConfig;
use crate::graphql::Operation;
use crate::oidc::OidcVerifier;
use crate::sessions::{ID_TOKEN_LIFETIME, IssuedTokens, SessionStore, TokenError};

pub struct AppState {
    pub verifier: OidcVerifier,
    pub sessions: SessionStore,
    pub required_group: Option<String>,
}

impl AppState {
    pub fn from_config(config: &ServerConfig) -> Self {
        Self {
            verifier: OidcVerifier::new(&config.oidc.issuer, &config.oidc.client_id),
            sessions: SessionStore::new(
                config.session.signing_secret.as_deref().map(str::as_bytes),
                config.session.max_age(),
            ),
            required_group: config.oidc.required_group.clone(),
        }
    }
}

/// Settings for a server embedded in the app.
pub struct EmbeddedConfig {
    pub issuer: String,
    pub client_id: String,
    /// Signs the server's tokens; persist it so users stay signed in across app restarts.
    pub signing_secret: Vec<u8>,
    pub session_max_age: Duration,
}

/// Starts the server on its own thread, listening on an OS-assigned `127.0.0.1` port, and
/// returns that address. The port is bound before returning, so requests made right away queue
/// until the server is serving.
pub fn spawn_embedded(config: EmbeddedConfig) -> anyhow::Result<SocketAddr> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .context("failed to bind the embedded server")?;
    listener
        .set_nonblocking(true)
        .context("failed to configure the embedded server listener")?;
    let address = listener.local_addr()?;
    let state = Arc::new(AppState {
        verifier: OidcVerifier::new(&config.issuer, &config.client_id),
        sessions: SessionStore::new(Some(&config.signing_secret), config.session_max_age),
        required_group: None,
    });
    std::thread::Builder::new()
        .name("embedded-selfhost-server".to_owned())
        .spawn(move || {
            // Agent turns mostly wait on the model API, so a few workers serve many sessions;
            // more cores still help when several turns stream at once.
            let workers =
                std::thread::available_parallelism().map_or(2, |cores| cores.get().clamp(2, 8));
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(workers)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    log::error!("Couldn't start the embedded server runtime: {error:#}");
                    return;
                }
            };
            runtime.block_on(async move {
                let result = async {
                    let listener = tokio::net::TcpListener::from_std(listener)?;
                    axum::serve(listener, router(state)).await
                }
                .await;
                if let Err(error) = result {
                    log::error!("The embedded server stopped: {error:#}");
                }
            });
        })
        .context("failed to start the embedded server thread")?;
    Ok(address)
}

const MAX_REQUEST_BYTES: usize = 256 * 1024 * 1024;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/auth/sso/exchange", post(exchange_provider_token))
        .route(
            "/v1/accounts:signInWithCustomToken",
            post(sign_in_with_custom_token),
        )
        .route("/v1/token", post(refresh_token))
        .route("/graphql/v2", post(graphql))
        .route("/ai/multi-agent", post(agent::multi_agent))
        .route("/ai/passive-suggestions", post(agent::passive_suggestions))
        // Agent requests carry the whole conversation, tool output included, so they outgrow
        // axum's 2 MB default.
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .with_state(state)
}

#[derive(Deserialize)]
struct ExchangeRequest {
    id_token: String,
}

async fn exchange_provider_token(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ExchangeRequest>,
) -> Response {
    let claims = match state.verifier.verify(&request.id_token).await {
        Ok(claims) => claims,
        Err(error) => {
            log::warn!("Rejected an identity provider token: {error:#}");
            return (StatusCode::UNAUTHORIZED, "invalid identity provider token").into_response();
        }
    };
    if let Some(group) = &state.required_group
        && !claims.groups.contains(group)
    {
        log::warn!(
            "Refused sign-in for {}: not in the required group",
            claims.email().unwrap_or("<no email>")
        );
        return (StatusCode::FORBIDDEN, "not in the required group").into_response();
    }
    log::info!("Signed in {}", claims.email().unwrap_or("<no email>"));
    let custom_token = state.sessions.sign_in(
        &claims.iss,
        &claims.sub,
        claims.email().map(str::to_owned),
        claims.name.clone(),
    );
    Json(json!({ "custom_token": custom_token })).into_response()
}

#[derive(Deserialize)]
struct CustomTokenForm {
    token: String,
}

/// Mirrors Firebase's `accounts:signInWithCustomToken`, which answers in camelCase.
async fn sign_in_with_custom_token(
    State(state): State<Arc<AppState>>,
    Form(form): Form<CustomTokenForm>,
) -> Response {
    match state.sessions.redeem_custom_token(&form.token).await {
        Ok(IssuedTokens {
            id_token,
            refresh_token,
        }) => Json(json!({
            "idToken": id_token,
            "refreshToken": refresh_token,
            "expiresIn": ID_TOKEN_LIFETIME.as_secs().to_string(),
        }))
        .into_response(),
        Err(error) => firebase_error(error),
    }
}

#[derive(Deserialize)]
struct RefreshForm {
    grant_type: String,
    refresh_token: String,
}

/// Mirrors Firebase's secure-token refresh endpoint, which answers in snake_case.
async fn refresh_token(
    State(state): State<Arc<AppState>>,
    Form(form): Form<RefreshForm>,
) -> Response {
    if form.grant_type != "refresh_token" {
        return firebase_error(TokenError::InvalidRefreshToken);
    }
    match state.sessions.refresh(&form.refresh_token) {
        Ok(IssuedTokens {
            id_token,
            refresh_token,
        }) => Json(json!({
            "id_token": id_token,
            "refresh_token": refresh_token,
            "expires_in": ID_TOKEN_LIFETIME.as_secs().to_string(),
            "token_type": "Bearer",
        }))
        .into_response(),
        Err(error) => firebase_error(error),
    }
}

fn firebase_error(error: TokenError) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": { "code": 400, "message": error.firebase_message() } })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct GraphqlQuery {
    op: Option<String>,
}

async fn graphql(
    State(state): State<Arc<AppState>>,
    Query(query): Query<GraphqlQuery>,
    headers: HeaderMap,
) -> Response {
    let operation = query.op.unwrap_or_default();
    let user = match bearer_token(&headers) {
        Some(token) => state.sessions.user_for_id_token(token),
        None => None,
    };
    match (Operation::from_name(&operation), user) {
        (Some(operation), Some(user)) => Json(operation.response(&user)).into_response(),
        (Some(_), None) => (StatusCode::UNAUTHORIZED, "sign in required").into_response(),
        (None, _) => {
            log::debug!("Unsupported GraphQL operation {operation}");
            Json(graphql::unsupported_operation_response(&operation)).into_response()
        }
    }
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
