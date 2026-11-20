//! A self-hosted replacement for the parts of Warp's server that sign users in: it trades an
//! organization identity provider's ID token for the client's account tokens and answers the
//! client's user query. State is held in memory.

pub mod config;
pub mod graphql;
pub mod oidc;
pub mod sessions;

use std::sync::Arc;

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::config::ServerConfig;
use crate::oidc::OidcVerifier;
use crate::sessions::{ID_TOKEN_LIFETIME, IssuedTokens, SessionStore, TokenError};

pub struct AppState {
    pub verifier: OidcVerifier,
    pub sessions: SessionStore,
    pub required_group: Option<String>,
}

impl AppState {
    pub async fn from_config(config: &ServerConfig) -> anyhow::Result<Self> {
        Ok(Self {
            verifier: OidcVerifier::discover(&config.oidc.issuer, &config.oidc.client_id).await?,
            sessions: SessionStore::new(
                config.session.signing_secret.as_deref(),
                config.session.max_age(),
            ),
            required_group: config.oidc.required_group.clone(),
        })
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/auth/sso/exchange", post(exchange_provider_token))
        .route(
            "/v1/accounts:signInWithCustomToken",
            post(sign_in_with_custom_token),
        )
        .route("/v1/token", post(refresh_token))
        .route("/graphql/v2", post(graphql))
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
    let custom_token = state
        .sessions
        .sign_in(
            &claims.iss,
            &claims.sub,
            claims.email().map(str::to_owned),
            claims.name.clone(),
        )
        .await;
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
    match state.sessions.refresh(&form.refresh_token).await {
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
        Some(token) => state.sessions.user_for_id_token(token).await,
        None => None,
    };
    match (operation.as_str(), user) {
        ("GetUser", Some(user)) => Json(graphql::get_user_response(&user)).into_response(),
        ("GetUser", None) => (StatusCode::UNAUTHORIZED, "sign in required").into_response(),
        (operation, _) => {
            log::debug!("Unsupported GraphQL operation {operation}");
            Json(graphql::unsupported_operation_response(operation)).into_response()
        }
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
