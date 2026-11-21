//! OAuth flow for connecting an xAI / Grok subscription (e.g. SuperGrok) to
//! Warp, so users can "plug in" their subscription instead of pasting a
//! pay-as-you-go API key.
//!
//! This mirrors the public Grok-CLI desktop OAuth flow: an OAuth 2.0
//! Authorization Code grant with PKCE and a fixed loopback redirect URI. xAI's
//! auth server only accepts the loopback redirect for an allowlisted
//! `client_id` bound to a specific port, so we reuse the Grok-CLI client and
//! bind the callback server to that exact port.
//!
//! Some browsers/networks can't reach the loopback callback (e.g. Private
//! Network Access is blocked), in which case xAI's consent screen instead
//! *displays* the authorization code for the user to paste back into the app.
//! [`OauthAttempt::manual_code_exchange`] supports that fallback by capturing
//! the attempt's PKCE verifier so a pasted code can be exchanged directly,
//! without ever observing the loopback redirect.
//!
//! This module owns only the network/protocol side: building the authorize
//! URL, running the loopback callback server, and exchanging/refreshing tokens
//! at xAI's token endpoint. Persistence of the resulting tokens, proactive
//! refresh scheduling, and injection into the request live in the parent
//! [`crate::grok_subscription`] module (refresh orchestration) and
//! [`crate::api_keys::ApiKeyManager`] (storage + request injection).

use std::future::Future;
use std::time::Duration;

use anyhow::{Context as _, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
pub use oauth_loopback::{
    CancellationHandle as OauthCancellationHandle, ReleaseSignal as OauthReleaseSignal,
};
use oauth_loopback::{LoopbackConfig, LoopbackListener};
use rand::RngCore as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const AUTHORIZE_URL: &str = "https://auth.x.ai/oauth2/authorize";
const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";

const REDIRECT_HOST: &str = "127.0.0.1";
const REDIRECT_PORT: u16 = 56121;

/// How long we keep the loopback server open waiting for the user to approve
/// the consent screen in their browser.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

const LOOPBACK_CONFIG: LoopbackConfig = LoopbackConfig {
    label: "Grok",
    port: REDIRECT_PORT,
    redirect_host: REDIRECT_HOST,
    timeout: CALLBACK_TIMEOUT,
    // xAI's browser consent screen fetches the loopback callback from these
    // origins rather than navigating to it.
    cors_allowed_origins: &["https://accounts.x.ai", "https://auth.x.ai"],
    success_html: SUCCESS_HTML,
    failure_html: FAILURE_HTML,
};

fn redirect_uri() -> String {
    format!("http://{REDIRECT_HOST}:{REDIRECT_PORT}/callback")
}

/// One in-flight OAuth login attempt: the bound loopback callback listener
/// plus the per-attempt PKCE/CSRF secrets, which never leave this module.
///
/// Construct with [`OauthAttempt::start`], open [`OauthAttempt::authorize_url`]
/// in the browser, then await [`OauthAttempt::finish`] to obtain tokens. Tying
/// the secrets to the attempt guarantees the same PKCE verifier and CSRF state
/// are used for both the authorize URL and the code exchange.
pub struct OauthAttempt {
    listener: LoopbackListener,
    pkce: PkceParams,
    cancellation: OauthCancellationHandle,
}

impl OauthAttempt {
    /// Binds the loopback callback server and generates fresh per-attempt
    /// secrets. Call this before opening the browser so a bind failure (e.g.
    /// another login already in progress, or Grok-CLI holding the port)
    /// surfaces before a browser tab opens.
    pub fn start() -> anyhow::Result<Self> {
        Ok(Self {
            listener: LoopbackListener::bind(LOOPBACK_CONFIG)?,
            pkce: PkceParams::generate(),
            cancellation: OauthCancellationHandle::new(),
        })
    }

    /// The authorization URL the user's browser should open to begin the flow.
    pub fn authorize_url(&self) -> String {
        authorize_url(&self.pkce)
    }

    /// Runs the rest of the browser-based PKCE flow: waits for the loopback
    /// callback, validates the CSRF state, and exchanges the authorization
    /// code for tokens. Consumes the attempt so its secrets can't be reused.
    ///
    /// Returns an [`OauthReleaseSignal`] alongside the result future so a
    /// caller that only needs to know the port is free again -- to allow a
    /// retry -- doesn't have to wait for a token exchange that a raced-in
    /// callback may have already started.
    pub fn finish(
        self,
    ) -> (
        OauthReleaseSignal,
        impl Future<Output = anyhow::Result<TokenResponse>>,
    ) {
        let PkceParams {
            verifier, state, ..
        } = self.pkce;
        let (release, code) = self.listener.wait_for_code(state, self.cancellation);
        let result = async move { exchange_code_for_tokens(&code.await?, &verifier).await };
        (release, result)
    }

    /// Clones the PKCE verifier for the pasted-code fallback while the
    /// loopback flow continues racing in parallel.
    pub fn manual_code_exchange(&self) -> ManualCodeExchange {
        ManualCodeExchange {
            verifier: self.pkce.verifier.clone(),
        }
    }

    pub fn cancellation_handle(&self) -> OauthCancellationHandle {
        self.cancellation.clone()
    }
}

/// Completes OAuth from a manually-pasted authorization code.
///
/// There is no redirect `state` to validate in this out-of-band path; PKCE
/// protects the exchange.
#[derive(Clone)]
pub struct ManualCodeExchange {
    verifier: String,
}

impl ManualCodeExchange {
    /// Exchanges a user-pasted authorization `code` with the attempt's PKCE verifier.
    pub async fn exchange(&self, code: &str) -> anyhow::Result<TokenResponse> {
        let code = code.trim();
        if code.is_empty() {
            bail!("enter the code shown in your browser to finish connecting");
        }
        exchange_code_for_tokens(code, &self.verifier).await
    }
}

/// The per-attempt secrets for one authorization request: the PKCE
/// verifier/challenge pair and the CSRF `state` value.
struct PkceParams {
    verifier: String,
    challenge: String,
    /// CSRF token echoed back on the redirect and validated against the
    /// response before the code is exchanged.
    state: String,
}

impl PkceParams {
    /// Generates a fresh PKCE verifier + S256 challenge and a random CSRF state.
    fn generate() -> Self {
        let verifier = random_url_safe_token();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_url_safe_token();
        Self {
            verifier,
            challenge,
            state,
        }
    }
}

/// Returns a URL-safe, unpadded base64 string of 32 random bytes. This is used
/// for both the PKCE code verifier (RFC 7636 allows 43-128 chars from the
/// unreserved set) and the CSRF state.
fn random_url_safe_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Builds the authorization URL the user's browser should open to begin the
/// flow.
fn authorize_url(pkce: &PkceParams) -> String {
    let redirect = redirect_uri();
    // `plan=generic` opts the consent screen into xAI's generic OAuth plan tier
    // (required for loopback OAuth from non-allowlisted clients); `referrer`
    // is best-effort attribution in xAI's OAuth logs.
    let params: [(&str, &str); 9] = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", &redirect),
        ("scope", SCOPE),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        ("state", &pkce.state),
        ("plan", "generic"),
        ("referrer", "warp"),
    ];
    let query =
        serde_urlencoded::to_string(params).expect("static OAuth params are always serializable");
    format!("{AUTHORIZE_URL}?{query}")
}

/// The token endpoint's response. Fields beyond `access_token` are optional
/// because xAI does not always return them. Other response fields (e.g.
/// `token_type`, `scope`) are ignored since nothing consumes them.
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
}

/// Exchanges the authorization code for OAuth tokens at xAI's token endpoint.
async fn exchange_code_for_tokens(code: &str, verifier: &str) -> anyhow::Result<TokenResponse> {
    let redirect = redirect_uri();
    let form: [(&str, &str); 5] = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", &redirect),
        ("client_id", CLIENT_ID),
        ("code_verifier", verifier),
    ];
    post_token_request(&form).await
}

/// Exchanges a previously obtained refresh token for a fresh set of tokens via
/// the OAuth 2.0 `refresh_token` grant. Used to keep the connected Grok
/// subscription's access token valid without re-running the browser flow.
///
/// xAI may or may not return a new `refresh_token`; callers should fall back to
/// the existing one when [`TokenResponse::refresh_token`] is `None` (rotation is
/// optional in OAuth 2.0).
pub async fn refresh_access_token(refresh_token: &str) -> anyhow::Result<TokenResponse> {
    let form: [(&str, &str); 3] = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", CLIENT_ID),
    ];
    post_token_request(&form).await
}

/// POSTs a form-encoded body to xAI's token endpoint and parses the
/// [`TokenResponse`]. Shared by the initial code exchange and refresh grants.
async fn post_token_request<T: serde::Serialize + ?Sized>(
    form: &T,
) -> anyhow::Result<TokenResponse> {
    let response = http_client::Client::new()
        .post(TOKEN_URL)
        .form(form)
        .send()
        .await
        .context("failed to send the Grok token request")?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("Grok token request failed ({status}): {body}");
    }

    response
        .json::<TokenResponse>()
        .await
        .context("failed to parse the Grok token response")
}

const SUCCESS_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Warp — Grok connected</title></head>\
<body style=\"font-family:system-ui,-apple-system,sans-serif;text-align:center;padding:3rem\">\
<h1>Grok connected</h1><p>You can close this window and return to Warp.</p></body></html>";

const FAILURE_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Warp — Grok authorization failed</title></head>\
<body style=\"font-family:system-ui,-apple-system,sans-serif;text-align:center;padding:3rem\">\
<h1>Authorization failed</h1><p>Something went wrong. Return to Warp and try again.</p></body></html>";

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;
