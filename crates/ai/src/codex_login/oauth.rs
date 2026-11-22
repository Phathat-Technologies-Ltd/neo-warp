//! Device login, token refresh, and the model list for a ChatGPT plan, over the Codex CLI's
//! OAuth client and ChatGPT's Codex backend.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use instant::Instant;
use serde::Deserialize;
use serde_json::Value;
use warpui_core::r#async::Timer;

use super::CODEX_API_URL;

const ISSUER: &str = "https://auth.openai.com";
/// The Codex CLI's public OAuth client.
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Where the user enters the device code.
pub const VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";
/// The backend filters its model list by client version; this is a Codex CLI release it serves.
const MODELS_CLIENT_VERSION: &str = "0.170.0";
/// `auth.openai.com` sits behind a bot filter that rejects HTTP clients' default agents.
const USER_AGENT: &str = concat!("neo-warp/", env!("CARGO_PKG_VERSION"));
const DEVICE_CODE_LIFETIME: Duration = Duration::from_secs(15 * 60);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// A pending device login: the code the user enters at [`VERIFICATION_URL`].
#[derive(Clone, Debug)]
pub struct DeviceCode {
    device_auth_id: String,
    pub user_code: String,
    interval: Duration,
}

#[derive(Deserialize)]
struct UserCodeResponse {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    /// Seconds, sent as a string or a number.
    #[serde(default)]
    interval: Option<Value>,
}

#[derive(Deserialize)]
struct DeviceGrant {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct ModelList {
    #[serde(default)]
    models: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    slug: String,
    #[serde(default)]
    visibility: Option<String>,
}

pub async fn request_device_code() -> Result<DeviceCode> {
    let response = http_client::Client::new()
        .post(format!("{ISSUER}/api/accounts/deviceauth/usercode"))
        .header("User-Agent", USER_AGENT)
        .json(&serde_json::json!({ "client_id": CLIENT_ID }))
        .send()
        .await
        .context("failed to reach OpenAI")?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("OpenAI refused to start a device login ({status}): {body}");
    }
    let start: UserCodeResponse =
        serde_json::from_str(&body).context("unexpected device login response")?;
    let interval = start
        .interval
        .and_then(|value| match value {
            Value::Number(number) => number.as_u64(),
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        })
        .filter(|seconds| *seconds > 0)
        .map_or(DEFAULT_POLL_INTERVAL, Duration::from_secs);
    Ok(DeviceCode {
        device_auth_id: start.device_auth_id,
        user_code: start.user_code,
        interval,
    })
}

/// Waits for the user to approve `code`, then trades the grant for tokens. Stops early once
/// `cancelled` is set.
pub async fn wait_for_approval(
    code: DeviceCode,
    cancelled: Arc<AtomicBool>,
) -> Result<TokenResponse> {
    let deadline = Instant::now() + DEVICE_CODE_LIFETIME;
    let grant = loop {
        if cancelled.load(Ordering::Relaxed) {
            bail!("the ChatGPT login was cancelled");
        }
        let response = http_client::Client::new()
            .post(format!("{ISSUER}/api/accounts/deviceauth/token"))
            .header("User-Agent", USER_AGENT)
            .json(&serde_json::json!({
                "device_auth_id": code.device_auth_id,
                "user_code": code.user_code,
            }))
            .send()
            .await
            .context("failed to reach OpenAI")?;
        let status = response.status();
        // 403 and 404 mean the user hasn't approved yet.
        if status.is_success() {
            break response
                .json::<DeviceGrant>()
                .await
                .context("unexpected device login grant")?;
        }
        if status.as_u16() != 403 && status.as_u16() != 404 {
            let body = response.text().await.unwrap_or_default();
            bail!("OpenAI refused the device login ({status}): {body}");
        }
        if Instant::now() >= deadline {
            bail!("the device code expired before it was approved");
        }
        Timer::after(code.interval).await;
    };
    post_token_request(&[
        ("grant_type", "authorization_code"),
        ("code", &grant.authorization_code),
        ("redirect_uri", &format!("{ISSUER}/deviceauth/callback")),
        ("client_id", CLIENT_ID),
        ("code_verifier", &grant.code_verifier),
    ])
    .await
}

pub async fn refresh_access_token(refresh_token: &str) -> Result<TokenResponse> {
    post_token_request(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", CLIENT_ID),
    ])
    .await
}

async fn post_token_request(form: &[(&str, &str)]) -> Result<TokenResponse> {
    let response = http_client::Client::new()
        .post(format!("{ISSUER}/oauth/token"))
        .header("User-Agent", USER_AGENT)
        .form(form)
        .send()
        .await
        .context("failed to reach OpenAI")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("OpenAI's token request failed ({status}): {body}");
    }
    response
        .json::<TokenResponse>()
        .await
        .context("unexpected token response")
}

/// The models the signed-in ChatGPT account may pick, in the backend's order.
pub async fn fetch_models(access_token: String) -> Result<Vec<String>> {
    let client = http_client::Client::new();
    let mut request = client
        .get(format!(
            "{CODEX_API_URL}/models?client_version={MODELS_CLIENT_VERSION}"
        ))
        .header("User-Agent", USER_AGENT)
        .bearer_auth(&access_token);
    if let Some(account_id) = account_id(&access_token) {
        request = request.header("chatgpt-account-id", account_id);
    }
    let response = request.send().await.context("failed to reach ChatGPT")?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("ChatGPT refused to list models ({status}): {body}");
    }
    parse_models(&body).context("ChatGPT returned an unexpected model list")
}

fn parse_models(body: &str) -> Result<Vec<String>> {
    let list: ModelList = serde_json::from_str(body)?;
    Ok(list
        .models
        .into_iter()
        .filter(|model| model.visibility.as_deref().is_none_or(|v| v == "list"))
        .map(|model| model.slug)
        .filter(|slug| !slug.trim().is_empty())
        .collect())
}

/// The ChatGPT account (workspace) an access token was issued for, from its unverified claims.
pub fn account_id(access_token: &str) -> Option<String> {
    let payload = access_token.split('.').nth(1)?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    claims
        .get("chatgpt_account_id")
        .or_else(|| claims.pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;
