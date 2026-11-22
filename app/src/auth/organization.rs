//! Organization sign-in against a self-hosted server, configured by an IT-deployed
//! `organization.toml` (see `crates/org_sign_in`). Without a `server_url`, the server runs
//! embedded in the app (see `crates/selfhost_server`).

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use org_flags::{OrgFlags, ProductArea};
use org_sign_in::{OrganizationConfig, OrganizationSignInConfig};
use warp_core::channel::ChannelState;
use warp_core::features::FeatureFlag;

use crate::server::server_api::auth::UserAuthenticationError;

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
    if config.sign_in.is_some() {
        // Before the embedded server starts, since the voice input flag decides whether it
        // transcribes locally.
        apply_org_flags(&OrgFlags::load(ChannelState::enable_debug_features()));
    }
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
    if config.sign_in.is_some()
        && !(config.server_url.is_none() && bundled_remote_server_builds().is_some())
    {
        // The SSH extension installs a Linux build of the app on the remote host, downloaded from
        // the server; only the embedded server, from builds bundled with the app, offers them.
        FeatureFlag::SshRemoteServer.set_enabled(false);
    }
    let _ = ORGANIZATION_CONFIG.set(config);
}

/// Sets each product-area flag from the organization's flags, and turns off the upstream flags
/// that gate parts of an area that is off.
fn apply_org_flags(flags: &OrgFlags) {
    for area in ProductArea::ALL {
        let enabled = flags.is_enabled(area);
        log::info!(
            "Organization flag {} is {}",
            area.key(),
            if enabled { "on" } else { "off" }
        );
        let (flag, upstream_flags) = feature_flags_for(area);
        flag.set_enabled(enabled);
        if !enabled {
            for upstream in upstream_flags {
                upstream.set_enabled(false);
            }
        }
    }
}

fn feature_flags_for(area: ProductArea) -> (FeatureFlag, &'static [FeatureFlag]) {
    match area {
        ProductArea::WarpDrive => (
            FeatureFlag::WarpDrive,
            &[FeatureFlag::DriveObjectsAsContext],
        ),
        ProductArea::SettingsSync => (FeatureFlag::SettingsSync, &[]),
        ProductArea::CloudAgents => (
            FeatureFlag::CloudAgents,
            &[
                FeatureFlag::CloudMode,
                FeatureFlag::CloudModeFromLocalSession,
                FeatureFlag::CloudModeHostSelector,
                FeatureFlag::CloudModeSetupV2,
                FeatureFlag::CloudModeInputV2,
                FeatureFlag::CloudModeImageContext,
                FeatureFlag::AmbientAgentsCommandLine,
                FeatureFlag::AmbientAgentsRTC,
                FeatureFlag::AmbientAgentsImageUpload,
                FeatureFlag::ScheduledAmbientAgents,
                FeatureFlag::CloudEnvironments,
                FeatureFlag::CreateEnvironmentSlashCommand,
                FeatureFlag::OzHandoff,
                FeatureFlag::HandoffLocalCloud,
                FeatureFlag::HandoffCloudCloud,
                FeatureFlag::PeriodicHandoffCheckpoints,
                FeatureFlag::OzLaunchModal,
                FeatureFlag::OzPlatformSkills,
                FeatureFlag::OzIdentityFederation,
                FeatureFlag::OzChangelogUpdates,
                FeatureFlag::CloudRunners,
                FeatureFlag::CloudAgentRunners,
            ],
        ),
        ProductArea::WarpHostedMcp => (
            FeatureFlag::WarpHostedMcp,
            &[
                FeatureFlag::FactoryMcp,
                FeatureFlag::PreviewUrlsMcp,
                FeatureFlag::WellKnownMcpIds,
                FeatureFlag::ManagedMcpCredentialRefresh,
            ],
        ),
        ProductArea::SharingAndTeams => (
            FeatureFlag::SharingAndTeams,
            &[
                FeatureFlag::CreatingSharedSessions,
                FeatureFlag::ViewingSharedSessions,
                FeatureFlag::SessionSharingAcls,
            ],
        ),
        ProductArea::BillingAndReferrals => (FeatureFlag::BillingAndReferrals, &[]),
        ProductArea::BlockTitles => (
            FeatureFlag::BlockTitles,
            &[FeatureFlag::SharedBlockTitleGeneration],
        ),
        ProductArea::ConversationCloudSync => (
            FeatureFlag::ConversationCloudSync,
            &[FeatureFlag::CloudConversations],
        ),
        ProductArea::VoiceInput => (FeatureFlag::VoiceInput, &[]),
        ProductArea::CodebaseIndexing => (FeatureFlag::CodebaseIndexing, &[]),
        ProductArea::AiPromptSuggestions => (FeatureFlag::AiPromptSuggestions, &[]),
        ProductArea::AiCommandSearch => (FeatureFlag::AiCommandSearch, &[]),
    }
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
        remote_server_artifacts: bundled_remote_server_builds(),
        transcriber: local_transcriber(),
        codebase_index_dir: Some(warp_core::paths::data_dir().join("codebase-index")),
    }) {
        Ok(address) => Some(format!("http://{address}")),
        Err(error) => {
            eprintln!("Error: couldn't start the embedded sign-in server: {error:#}");
            log::error!("Couldn't start the embedded sign-in server: {error:#}");
            None
        }
    }
}

/// The on-device speech engine for `/ai/transcribe`, when local voice input is on.
fn local_transcriber() -> Option<std::sync::Arc<dyn local_transcription::Transcriber>> {
    if !FeatureFlag::VoiceInput.is_enabled() {
        return None;
    }
    crate::voice::local_model::mark_served_locally();
    Some(std::sync::Arc::new(
        local_transcription::ParakeetTranscriber::new(crate::voice::local_model::model_store()),
    ))
}

/// The directory of Linux builds bundled with the app for its SSH extension, if any.
fn bundled_remote_server_builds() -> Option<PathBuf> {
    warp_core::paths::bundled_resources_dir()
        .map(|dir| dir.join("remote-server"))
        .filter(|dir| dir.is_dir())
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

/// The product name organization builds show in place of "Warp".
pub const PRODUCT_NAME: &str = "neo-warp";

pub const SIGN_IN_EXPIRED_MESSAGE: &str = "Your sign-in expired. Sign in again to continue.";

/// The message to show when signing in, or keeping a session, fails.
pub fn login_failure_message(err: &UserAuthenticationError) -> String {
    match err {
        // A refused refresh means the organization session ended, not a broken login.
        UserAuthenticationError::DeniedAccessToken(_)
        | UserAuthenticationError::UserAccountDisabled(_)
            if sign_in_config().is_some() =>
        {
            SIGN_IN_EXPIRED_MESSAGE.to_owned()
        }
        UserAuthenticationError::DeniedAccessToken(_)
        | UserAuthenticationError::UserAccountDisabled(_)
        | UserAuthenticationError::InvalidStateParameter
        | UserAuthenticationError::MissingStateParameter
        | UserAuthenticationError::DeviceCodeRequestTimedOut { .. }
        | UserAuthenticationError::OrganizationAccessDenied(_)
        | UserAuthenticationError::Unexpected(_) => format!("{err:#}"),
    }
}

/// The organization sign-in settings, when this build signs users in through their organization
/// instead of a Warp account.
pub fn model_specs_config() -> Option<&'static org_sign_in::ModelSpecsConfig> {
    ORGANIZATION_CONFIG
        .get()
        .and_then(|config| config.model_specs.as_ref())
}

pub fn sign_in_config() -> Option<&'static OrganizationSignInConfig> {
    ORGANIZATION_CONFIG.get()?.sign_in.as_ref()
}
