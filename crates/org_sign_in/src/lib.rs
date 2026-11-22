//! Organization sign-in: an OpenID Connect authorization-code flow with PKCE against the
//! company's identity provider (any OIDC provider; Entra, Okta, Google, Keycloak, ...), using a
//! public client and a loopback redirect, followed by an exchange of the provider's ID token for
//! a sign-in token at the self-hosted backend.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use oauth_loopback::{AuthorizationDenied, LoopbackConfig, LoopbackListener};
pub use oauth_loopback::{CancellationHandle, ReleaseSignal};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const ISSUER_ENV: &str = "WARP_ORG_SIGN_IN_ISSUER";
const CLIENT_ID_ENV: &str = "WARP_ORG_SIGN_IN_CLIENT_ID";
const DISPLAY_NAME_ENV: &str = "WARP_ORG_SIGN_IN_DISPLAY_NAME";
const EXTRA_SCOPES_ENV: &str = "WARP_ORG_SIGN_IN_SCOPES";
const REDIRECT_PORT_ENV: &str = "WARP_ORG_SIGN_IN_REDIRECT_PORT";
const SERVER_URL_ENV: &str = "WARP_ORG_SERVER_URL";
const CONFIG_FILE_ENV: &str = "WARP_ORG_CONFIG_FILE";
const ARTIFICIAL_ANALYSIS_KEY_ENV: &str = "WARP_ORG_ARTIFICIAL_ANALYSIS_API_KEY";

const BASE_SCOPES: &str = "openid profile email offline_access";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);
const BACKEND_EXCHANGE_PATH: &str = "/auth/sso/exchange";

/// Settings for signing in through the organization's identity provider.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct OrganizationSignInConfig {
    /// The provider's OIDC issuer URL, e.g. `https://login.microsoftonline.com/{tenant}/v2.0`.
    pub issuer: String,
    /// The public client (application) ID registered with the provider.
    pub client_id: String,
    /// Provider name shown on the sign-in button, e.g. `Microsoft`.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Scopes requested in addition to `openid profile email offline_access`.
    #[serde(default)]
    pub extra_scopes: Vec<String>,
    /// Fixed loopback port for providers that require the exact registered redirect port.
    #[serde(default)]
    pub redirect_port: Option<u16>,
}

/// Organization settings: the self-hosted server and how its users sign in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct OrganizationConfig {
    /// Root URL of the self-hosted server that replaces Warp's server.
    #[serde(default)]
    pub server_url: Option<String>,
    #[serde(default)]
    pub sign_in: Option<OrganizationSignInConfig>,
    #[serde(default)]
    pub model_specs: Option<ModelSpecsConfig>,
    #[serde(default)]
    pub recording: RecordingConfig,
    #[serde(default)]
    pub mcp_oauth: McpOAuthConfig,
}

/// How MCP servers' OAuth sign-in redirects back to the app.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct McpOAuthConfig {
    /// Fixed loopback port, for gateways that only approve an exact redirect URI. Unset means
    /// an OS-assigned port per sign-in.
    #[serde(default)]
    pub redirect_port: Option<u16>,
    /// Host in the loopback redirect URI.
    #[serde(default)]
    pub redirect_host: McpOAuthRedirectHost,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
pub enum McpOAuthRedirectHost {
    #[default]
    #[serde(rename = "127.0.0.1")]
    Ipv4Literal,
    #[serde(rename = "localhost")]
    Localhost,
}

/// Session recording policy.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct RecordingConfig {
    /// Extra regular expressions whose matches are replaced with `[REDACTED]` in recordings,
    /// on top of the built-in set.
    #[serde(default)]
    pub redact_patterns: Vec<String>,
}

/// Where the model picker's intelligence, speed, and cost ratings come from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct ModelSpecsConfig {
    /// A key for Artificial Analysis's free Data API.
    pub artificial_analysis_api_key: String,
    /// Our model IDs mapped to Artificial Analysis slugs, for models matching by name misses.
    #[serde(default)]
    pub aliases: HashMap<String, String>,
}

impl OrganizationConfig {
    /// Loads the organization config from the system-wide `organization.toml`, which takes
    /// admin rights to write. With `allow_overrides` (development builds), `WARP_ORG_*`
    /// environment variables or a `WARP_ORG_CONFIG_FILE` take precedence; shipped builds ignore
    /// them so a user can't be talked into redirecting the app.
    pub fn load(allow_overrides: bool) -> anyhow::Result<Self> {
        if allow_overrides {
            let from_env = Self::from_lookup(|key| std::env::var(key).ok())?;
            if from_env != Self::default() {
                return Ok(from_env);
            }
            if let Some(path) = std::env::var_os(CONFIG_FILE_ENV) {
                return Self::from_file(Path::new(&path));
            }
        }
        match default_config_file_path() {
            Some(path) => Self::from_file(&path),
            None => Ok(Self::default()),
        }
    }

    /// Reads an organization config file. A missing file means nothing is configured.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(
                    anyhow::Error::new(error).context(format!("failed to read {}", path.display()))
                );
            }
        };
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("{} is not a valid organization config", path.display()))?;
        if let Some(sign_in) = &config.sign_in
            && (sign_in.issuer.trim().is_empty() || sign_in.client_id.trim().is_empty())
        {
            anyhow::bail!(
                "{}: [sign_in] needs both `issuer` and `client_id`",
                path.display()
            );
        }
        if let Some(server_url) = &config.server_url
            && !(server_url.starts_with("https://") || server_url.starts_with("http://"))
        {
            anyhow::bail!("{}: `server_url` must be an http(s) URL", path.display());
        }
        Ok(config)
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        Ok(Self {
            server_url: lookup(SERVER_URL_ENV)
                .map(|url| url.trim().to_owned())
                .filter(|url| !url.is_empty()),
            sign_in: OrganizationSignInConfig::from_lookup(&lookup)?,
            model_specs: lookup(ARTIFICIAL_ANALYSIS_KEY_ENV)
                .map(|key| key.trim().to_owned())
                .filter(|key| !key.is_empty())
                .map(|artificial_analysis_api_key| ModelSpecsConfig {
                    artificial_analysis_api_key,
                    aliases: HashMap::new(),
                }),
            recording: RecordingConfig::default(),
            mcp_oauth: McpOAuthConfig::default(),
        })
    }
}

impl OrganizationSignInConfig {
    /// Reads `WARP_ORG_SIGN_IN_*` values. Returns `None` unless both the issuer and the client
    /// ID are set.
    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<Self>> {
        let get = |key| lookup(key).filter(|value| !value.trim().is_empty());
        let (Some(issuer), Some(client_id)) = (get(ISSUER_ENV), get(CLIENT_ID_ENV)) else {
            return Ok(None);
        };
        let redirect_port = get(REDIRECT_PORT_ENV)
            .map(|port| {
                port.trim()
                    .parse()
                    .with_context(|| format!("{REDIRECT_PORT_ENV} must be a port number"))
            })
            .transpose()?;
        Ok(Some(Self {
            issuer: issuer.trim().to_owned(),
            client_id: client_id.trim().to_owned(),
            display_name: get(DISPLAY_NAME_ENV).map(|name| name.trim().to_owned()),
            extra_scopes: get(EXTRA_SCOPES_ENV)
                .map(|scopes| scopes.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default(),
            redirect_port,
        }))
    }

    /// The sign-in button label.
    pub fn button_label(&self) -> String {
        match &self.display_name {
            Some(name) => format!("Sign in with {name}"),
            None => "Sign in with your organization".to_owned(),
        }
    }

    fn scope(&self) -> String {
        std::iter::once(BASE_SCOPES)
            .chain(self.extra_scopes.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The system-wide location IT deploys the organization config file to.
fn default_config_file_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some(PathBuf::from(
            "/Library/Application Support/Warp/organization.toml",
        ))
    } else if cfg!(target_os = "windows") {
        std::env::var_os("ProgramData").map(|program_data| {
            PathBuf::from(program_data)
                .join("Warp")
                .join("organization.toml")
        })
    } else {
        Some(PathBuf::from("/etc/warp/organization.toml"))
    }
}

/// The endpoints read from the provider's discovery document.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ProviderMetadata {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
}

/// Why organization sign-in failed, split by what the user can do about it.
#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    /// The identity provider or the backend refused this user.
    #[error("access denied: {0}")]
    AccessDenied(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Fetches `{issuer}/.well-known/openid-configuration`.
pub async fn discover(issuer: &str) -> anyhow::Result<ProviderMetadata> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let response = http_client::Client::new_external()
        .get(&url)
        .send()
        .await
        .with_context(|| format!("failed to fetch the identity provider configuration at {url}"))?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("the identity provider configuration at {url} returned {status}");
    }
    response
        .json::<ProviderMetadata>()
        .await
        .context("failed to parse the identity provider configuration")
}

/// One in-flight sign-in: the bound loopback listener plus the per-attempt PKCE, CSRF, and
/// replay secrets, which never leave this module.
///
/// Start it, open [`SignInAttempt::authorize_url`] in the browser, then await
/// [`SignInAttempt::finish`] for the provider's ID token.
pub struct SignInAttempt {
    listener: LoopbackListener,
    secrets: AttemptSecrets,
    client_id: String,
    scope: String,
    metadata: ProviderMetadata,
    cancellation: CancellationHandle,
}

struct AttemptSecrets {
    verifier: String,
    challenge: String,
    state: String,
    nonce: String,
}

impl AttemptSecrets {
    fn generate() -> Self {
        let verifier = random_url_safe_token();
        Self {
            challenge: URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            verifier,
            state: random_url_safe_token(),
            nonce: random_url_safe_token(),
        }
    }
}

/// 32 random bytes, URL-safe base64 without padding (43 characters, valid as a PKCE verifier).
fn random_url_safe_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

impl SignInAttempt {
    /// Binds the loopback listener. Call before opening the browser so a bind failure surfaces
    /// first.
    pub fn start(
        config: &OrganizationSignInConfig,
        metadata: ProviderMetadata,
    ) -> anyhow::Result<Self> {
        let listener = LoopbackListener::bind(LoopbackConfig {
            label: "Organization",
            port: config.redirect_port.unwrap_or(0),
            redirect_host: "localhost",
            timeout: CALLBACK_TIMEOUT,
            cors_allowed_origins: &[],
            success_html: SUCCESS_HTML,
            failure_html: FAILURE_HTML,
        })?;
        Ok(Self {
            listener,
            secrets: AttemptSecrets::generate(),
            client_id: config.client_id.clone(),
            scope: config.scope(),
            metadata,
            cancellation: CancellationHandle::new(),
        })
    }

    pub fn authorize_url(&self) -> String {
        let redirect_uri = self.listener.redirect_uri();
        let params = [
            ("client_id", self.client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", &redirect_uri),
            ("scope", &self.scope),
            ("code_challenge", &self.secrets.challenge),
            ("code_challenge_method", "S256"),
            ("state", &self.secrets.state),
            ("nonce", &self.secrets.nonce),
        ];
        let query =
            serde_urlencoded::to_string(params).expect("authorize params are always serializable");
        let separator = if self.metadata.authorization_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        format!("{}{separator}{query}", self.metadata.authorization_endpoint)
    }

    pub fn cancellation_handle(&self) -> CancellationHandle {
        self.cancellation.clone()
    }

    /// Waits for the browser redirect, exchanges the code, and checks the ID token's `nonce`.
    /// The [`ReleaseSignal`] fires as soon as the loopback port is free again.
    pub fn finish(
        self,
    ) -> (
        ReleaseSignal,
        impl Future<Output = Result<String, SignInError>>,
    ) {
        let redirect_uri = self.listener.redirect_uri();
        let Self {
            listener,
            secrets,
            client_id,
            metadata,
            cancellation,
            ..
        } = self;
        let (release, code) = listener.wait_for_code(secrets.state, cancellation);
        let result = async move {
            let code = code.await.map_err(classify_callback_error)?;
            let id_token = exchange_code(
                &metadata.token_endpoint,
                &client_id,
                &code,
                &redirect_uri,
                &secrets.verifier,
            )
            .await?;
            check_nonce(&id_token, &secrets.nonce)?;
            Ok(id_token)
        };
        (release, result)
    }
}

fn classify_callback_error(error: anyhow::Error) -> SignInError {
    match error.downcast_ref::<AuthorizationDenied>() {
        Some(denied) if denied.error == "access_denied" => SignInError::AccessDenied(
            denied
                .description
                .clone()
                .unwrap_or_else(|| denied.error.clone()),
        ),
        Some(_) | None => SignInError::Other(error),
    }
}

#[derive(Deserialize)]
struct ProviderTokenResponse {
    id_token: Option<String>,
}

async fn exchange_code(
    token_endpoint: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> anyhow::Result<String> {
    let form = [
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
    ];
    let response = http_client::Client::new_external()
        .post(token_endpoint)
        .form(&form)
        .send()
        .await
        .context("failed to send the identity provider token request")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("the identity provider token request failed ({status}): {body}");
    }
    response
        .json::<ProviderTokenResponse>()
        .await
        .context("failed to parse the identity provider token response")?
        .id_token
        .ok_or_else(|| anyhow!("the identity provider returned no ID token; is `openid` allowed?"))
}

/// Rejects an ID token whose `nonce` claim doesn't match this attempt. The backend verifies the
/// signature; this only binds the token to the attempt that requested it.
fn check_nonce(id_token: &str, expected: &str) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct Claims {
        nonce: Option<String>,
    }
    let payload = id_token
        .split('.')
        .nth(1)
        .ok_or_else(|| anyhow!("the ID token is not a JWT"))?;
    let claims: Claims = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .context("the ID token payload is not base64url")?,
    )
    .context("the ID token payload is not JSON")?;
    if claims.nonce.as_deref() != Some(expected) {
        anyhow::bail!("the ID token nonce did not match this sign-in attempt");
    }
    Ok(())
}

#[derive(Serialize)]
struct BackendExchangeRequest<'a> {
    id_token: &'a str,
}

#[derive(Deserialize)]
struct BackendExchangeResponse {
    custom_token: String,
}

/// Trades the provider's ID token for a sign-in token at the self-hosted backend. The result is
/// a custom token for the existing account flow.
pub async fn exchange_with_backend(
    server_root_url: &str,
    id_token: &str,
) -> Result<String, SignInError> {
    let url = format!(
        "{}{BACKEND_EXCHANGE_PATH}",
        server_root_url.trim_end_matches('/')
    );
    let response = http_client::Client::new()
        .post(&url)
        .json(&BackendExchangeRequest { id_token })
        .send()
        .await
        .with_context(|| format!("failed to reach the sign-in backend at {url}"))?;
    let status = response.status();
    if status == http_client::StatusCode::FORBIDDEN {
        return Err(SignInError::AccessDenied(
            "the sign-in backend refused this account".to_owned(),
        ));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("the sign-in backend returned {status}: {body}").into());
    }
    Ok(response
        .json::<BackendExchangeResponse>()
        .await
        .context("failed to parse the sign-in backend response")?
        .custom_token)
}

const SUCCESS_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Warp — signed in</title></head>\
<body style=\"font-family:system-ui,-apple-system,sans-serif;text-align:center;padding:3rem\">\
<h1>You're signed in</h1><p>You can close this window and return to Warp.</p></body></html>";

const FAILURE_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Warp — sign-in failed</title></head>\
<body style=\"font-family:system-ui,-apple-system,sans-serif;text-align:center;padding:3rem\">\
<h1>Sign-in failed</h1><p>Return to Warp and try again.</p></body></html>";

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
