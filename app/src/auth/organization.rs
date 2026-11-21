//! Organization sign-in against a self-hosted server, configured by an IT-deployed
//! `organization.toml` (see `crates/org_sign_in`). Without a `server_url`, the server runs
//! embedded in the app (see `crates/selfhost_server`).

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use org_sign_in::{OrganizationConfig, OrganizationSignInConfig};
use warp_core::channel::ChannelState;
use warp_core::features::FeatureFlag;

static ORGANIZATION_CONFIG: OnceLock<OrganizationConfig> = OnceLock::new();

/// How long an organization sign-in lasts before the user must pass the identity provider again.
const EMBEDDED_SESSION_MAX_AGE: Duration = Duration::from_secs(12 * 60 * 60);
const SIGNING_SECRET_FILE: &str = "selfhost-signing-secret";

/// Loads the organization config and points the app's server, websocket, and token-service URLs
/// at the self-hosted server: the configured `server_url`, or else a server embedded in this
/// process. Must run after feature flags are initialized and before any server client is
/// created.
pub(crate) fn init() {
    let config = if FeatureFlag::OrganizationSignIn.is_enabled() {
        // Shipped builds only trust the admin-written system file; see `OrganizationConfig::load`.
        OrganizationConfig::load(ChannelState::enable_debug_features()).unwrap_or_else(|error| {
            eprintln!("Error: ignoring the organization config: {error:#}");
            log::error!("Ignoring the organization config: {error:#}");
            OrganizationConfig::default()
        })
    } else {
        OrganizationConfig::default()
    };
    let server_url = match (&config.server_url, &config.sign_in) {
        (Some(server_url), _) => Some(server_url.trim_end_matches('/').to_owned()),
        (None, Some(sign_in)) => start_embedded_server(sign_in),
        (None, None) => None,
    };
    if let Some(server_url) = server_url
        && let Err(error) = apply_server_url(&server_url)
    {
        eprintln!("Error: invalid organization server URL {server_url}: {error:#}");
        log::error!("Invalid organization server URL {server_url}: {error:#}");
    }
    let _ = ORGANIZATION_CONFIG.set(config);
}

fn start_embedded_server(sign_in: &OrganizationSignInConfig) -> Option<String> {
    let signing_secret = load_or_create_signing_secret().unwrap_or_else(|error| {
        log::error!(
            "Couldn't persist the sign-in secret, so restarting signs users out: {error:#}"
        );
        selfhost_server::sessions::generate_signing_secret().to_vec()
    });
    match selfhost_server::spawn_embedded(selfhost_server::EmbeddedConfig {
        issuer: sign_in.issuer.clone(),
        client_id: sign_in.client_id.clone(),
        signing_secret,
        session_max_age: EMBEDDED_SESSION_MAX_AGE,
    }) {
        Ok(address) => Some(format!("http://{address}")),
        Err(error) => {
            eprintln!("Error: couldn't start the embedded sign-in server: {error:#}");
            log::error!("Couldn't start the embedded sign-in server: {error:#}");
            None
        }
    }
}

/// Reads the embedded server's token-signing secret, creating it on first use. Keeping it across
/// launches keeps users signed in.
fn load_or_create_signing_secret() -> anyhow::Result<Vec<u8>> {
    let path = signing_secret_path();
    match std::fs::read(&path) {
        Ok(secret) if secret.len() >= 32 => return Ok(secret),
        Ok(_) => log::warn!("Replacing a malformed sign-in secret at {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let secret = selfhost_server::sessions::generate_signing_secret();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&path)?.write_all(&secret)?;
    Ok(secret.to_vec())
}

fn signing_secret_path() -> PathBuf {
    warp_core::paths::secure_state_dir()
        .unwrap_or_else(warp_core::paths::state_dir)
        .join(SIGNING_SECRET_FILE)
}

fn apply_server_url(server_url: &str) -> Result<(), url::ParseError> {
    let ws_url = match server_url.split_once("://") {
        Some(("https", rest)) => format!("wss://{rest}/graphql/v2"),
        Some((_, rest)) => format!("ws://{rest}/graphql/v2"),
        None => format!("ws://{server_url}/graphql/v2"),
    };
    ChannelState::override_server_root_url(server_url.to_owned())?;
    ChannelState::override_ws_server_url(ws_url)?;
    ChannelState::override_auth_token_root_url(server_url.to_owned())?;
    log::info!("Using the self-hosted server at {server_url}");
    Ok(())
}

/// The organization sign-in settings, when this build signs users in through their organization
/// instead of a Warp account.
pub fn sign_in_config() -> Option<&'static OrganizationSignInConfig> {
    ORGANIZATION_CONFIG.get()?.sign_in.as_ref()
}
