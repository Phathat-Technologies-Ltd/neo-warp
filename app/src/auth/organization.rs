//! Organization sign-in against a self-hosted server, configured by an IT-deployed
//! `organization.toml` (see `crates/org_sign_in`).

use std::sync::OnceLock;

use org_sign_in::{OrganizationConfig, OrganizationSignInConfig};
use warp_core::channel::ChannelState;
use warp_core::features::FeatureFlag;

static ORGANIZATION_CONFIG: OnceLock<OrganizationConfig> = OnceLock::new();

/// Loads the organization config and, when it names a self-hosted server, points the app's
/// server, websocket, and token-service URLs at it. Must run after feature flags are initialized
/// and before any server client is created.
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
    if let Some(server_url) = &config.server_url
        && let Err(error) = apply_server_url(server_url.trim_end_matches('/'))
    {
        eprintln!("Error: invalid organization server_url {server_url}: {error:#}");
        log::error!("Invalid organization server_url {server_url}: {error:#}");
    }
    let _ = ORGANIZATION_CONFIG.set(config);
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
