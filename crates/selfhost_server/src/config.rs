use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;

/// The server's TOML configuration file.
#[derive(Clone, Debug, Deserialize)]
pub struct ServerConfig {
    /// Address to listen on.
    #[serde(default = "default_listen")]
    pub listen: String,
    pub oidc: OidcConfig,
    #[serde(default)]
    pub session: SessionConfig,
    /// Directory of the Linux builds the app installs on SSH hosts
    /// (`remote-server-linux-<arch>.tar.gz`).
    #[serde(default)]
    pub remote_server_artifacts: Option<PathBuf>,
    /// Where codebase indexes are stored; unset turns codebase indexing off.
    #[serde(default)]
    pub codebase_index_dir: Option<PathBuf>,
    /// Where session recordings are stored; unset turns `/recordings` off.
    #[serde(default)]
    pub recordings_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OidcConfig {
    /// The identity provider's issuer URL, as configured in the client.
    pub issuer: String,
    /// The public client ID registered with the provider; ID tokens must carry it as `aud`.
    pub client_id: String,
    /// When set, only users whose ID token lists this value in its `groups` claim may sign in.
    #[serde(default)]
    pub required_group: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionConfig {
    /// How long a sign-in lasts before the user must sign in with the provider again. This bounds
    /// how long a user removed in the provider keeps access.
    #[serde(default = "default_max_age_hours")]
    pub max_age_hours: u64,
    /// Secret for signing the server's own ID tokens. Without it a random one is generated at
    /// startup, so restarting the server signs everyone out.
    #[serde(default)]
    pub signing_secret: Option<String>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_age_hours: default_max_age_hours(),
            signing_secret: None,
        }
    }
}

impl SessionConfig {
    pub fn max_age(&self) -> Duration {
        Duration::from_secs(self.max_age_hours * 60 * 60)
    }
}

fn default_listen() -> String {
    "127.0.0.1:8090".to_owned()
}

fn default_max_age_hours() -> u64 {
    12
}

impl ServerConfig {
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        toml::from_str(&contents)
            .with_context(|| format!("{} is not a valid config", path.display()))
    }
}
