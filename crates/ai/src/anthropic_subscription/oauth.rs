//! OAuth flow for Anthropic / Claude (Pro, Max, Team) via Claude Code public client.
//!
//! Reuses the Grok subscription pattern: PKCE S256, public client_id, JSON token
//! endpoint. Primary UX is the hosted manual-code fallback (redirect_uri is
//! Anthropic's platform callback, browser shows CODE#STATE to paste), mirroring
//! `grok_subscription::oauth::ManualCodeExchange`. Loopback variant is omitted
//! for prototype — hosted flow is sufficient to prove viability and matches
//! Anthropic's current `https://console.anthropic.com/oauth/code/callback` guidance.
//!
//! Endpoints from docs.rs/modelbridge + coqu + opencode-anthropic-auth:
//!   authorize: https://claude.ai/oauth/authorize  (Max/Pro/Team)
//!   token:     https://console.anthropic.com/v1/oauth/token  (also platform.claude.com)
//!   redirect:  https://console.anthropic.com/oauth/code/callback
//!   client:    9d1c250a-e61b-44d9-88ed-5944d1962f5e (Claude Code public client)
//!   scope:     org:create_api_key user:profile user:inference  (+ sessions/mcp on newer APIs)
//!   state:     verifier (Anthropic non-standard: state == code_verifier)

use anyhow::{Context as _, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Max/Pro/Team subscription flow (claude.ai) — blocked by enterprise SSO gates.
pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
/// Console/API flow — works for enterprise orgs; same client_id, token endpoint, redirect.
/// This is the default for the prototype verifier to avoid the "Access managed by your organization" gate.
pub const CONSOLE_AUTHORIZE_URL: &str = "https://console.anthropic.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
pub const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
/// Includes `org:create_api_key` so token can also mint a persistent API key if desired;
/// `user:inference` covers direct subscription inference.
pub const SCOPE: &str = "org:create_api_key user:profile user:inference";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicOAuthFlow {
    /// claude.ai — Claude Code / Max subscription
    ClaudeCode,
    /// console.anthropic.com — API / Console org (avoids enterprise SSO block)
    Console,
}

impl AnthropicOAuthFlow {
    pub fn authorize_url(&self) -> &'static str {
        match self {
            Self::ClaudeCode => AUTHORIZE_URL,
            Self::Console => CONSOLE_AUTHORIZE_URL,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code (claude.ai)",
            Self::Console => "Console / API (console.anthropic.com)",
        }
    }
}

/// Token endpoint response — same shape as Grok but token type is Bearer + sk-ant-oat01.
#[derive(Debug, Deserialize, Clone)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Clone)]
pub struct PkceParams {
    pub verifier: String,
    pub challenge: String,
    /// Anthropic requires state == verifier
    pub state: String,
}

impl PkceParams {
    pub fn generate() -> Self {
        let verifier = random_url_safe_token();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            state: verifier.clone(),
            verifier,
            challenge,
        }
    }
}

fn random_url_safe_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Build authorize URL to open in browser. User copies CODE#STATE back.
pub fn authorize_url(pkce: &PkceParams) -> String {
    authorize_url_for_flow(pkce, AnthropicOAuthFlow::ClaudeCode)
}

pub fn authorize_url_for_flow(pkce: &PkceParams, flow: AnthropicOAuthFlow) -> String {
    let base = flow.authorize_url();
    let params: [(&str, &str); 8] = [
        ("code", "true"),
        ("client_id", CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPE),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        ("state", &pkce.state),
    ];
    let query =
        serde_urlencoded::to_string(params).expect("static Anthropic OAuth params serializable");
    format!("{base}?{query}")
}

/// Pasted-code exchange: supports `CODE` or `CODE#STATE` (platform page shows 92-char with #).
#[derive(Clone)]
pub struct ManualCodeExchange {
    pub verifier: String,
}

impl ManualCodeExchange {
    pub async fn exchange(&self, raw_code: &str) -> anyhow::Result<TokenResponse> {
        let code = raw_code.trim().split('#').next().unwrap_or("").trim();
        if code.is_empty() {
            bail!("enter the code shown in your browser to finish connecting Claude");
        }
        exchange_code_for_tokens(code, &self.verifier).await
    }
}

async fn exchange_code_for_tokens(code: &str, verifier: &str) -> anyhow::Result<TokenResponse> {
    // Anthropic expects JSON body with code, state (=verifier), code_verifier
    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "code": code,
        "state": verifier,
        "client_id": CLIENT_ID,
        "redirect_uri": REDIRECT_URI,
        "code_verifier": verifier,
    });
    post_token_request(&body.to_string()).await
}

pub async fn refresh_access_token(refresh_token: &str) -> anyhow::Result<TokenResponse> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLIENT_ID,
    });
    post_token_request(&body.to_string()).await
}

async fn post_token_request(json_body: &str) -> anyhow::Result<TokenResponse> {
    let response = http_client::Client::new()
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .body(json_body.to_owned())
        .send()
        .await
        .context("failed to send Anthropic token request")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("Anthropic token request failed ({status}): {body}");
    }
    response
        .json::<TokenResponse>()
        .await
        .context("failed to parse Anthropic token response")
}

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;
