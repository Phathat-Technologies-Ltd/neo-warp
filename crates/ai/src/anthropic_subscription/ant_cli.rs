//! Claude Console sign-in through Anthropic's own `ant` CLI (bundled with the app, or found on
//! `PATH`). `ant auth login` runs Anthropic's OAuth flow with Anthropic's client and keeps the
//! credentials in a config directory private to this app; `ant auth print-credentials` returns a
//! current access token, refreshing it when it is near expiry.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use command::r#async::Command;
use serde::Deserialize;

use super::oauth::TokenResponse;

static ANT_CLI: OnceLock<Option<AntCli>> = OnceLock::new();

const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug)]
pub struct AntCli {
    binary: PathBuf,
    config_dir: PathBuf,
}

/// Registers the `ant` binary to use, if any, and the config directory its credentials live in.
/// Only the first call takes effect.
pub fn configure(binary: Option<PathBuf>, config_dir: PathBuf) {
    let _ = ANT_CLI.set(binary.map(|binary| AntCli { binary, config_dir }));
}

/// The configured `ant` CLI, when one was found.
pub fn get() -> Option<&'static AntCli> {
    ANT_CLI.get()?.as_ref()
}

#[derive(Deserialize)]
struct Credentials {
    access_token: String,
    /// Unix seconds.
    expires_at: Option<u64>,
    scope: Option<String>,
}

impl AntCli {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .env("ANTHROPIC_CONFIG_DIR", &self.config_dir)
            // These would override the login for the commands run here.
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("ANTHROPIC_AUTH_TOKEN")
            .env_remove("ANTHROPIC_PROFILE")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    /// Fails when the binary isn't Anthropic's `ant`; Apache Ant, the Java build tool, has the
    /// same name.
    async fn check_is_anthropic_cli(&self) -> anyhow::Result<()> {
        let output = self
            .command()
            .arg("--version")
            .output()
            .await
            .with_context(|| format!("failed to run {}", self.binary.display()))?;
        if !String::from_utf8_lossy(&output.stdout).starts_with("ant version") {
            bail!(
                "{} is not Anthropic's ant CLI (it may be Apache Ant)",
                self.binary.display()
            );
        }
        Ok(())
    }

    /// Runs the browser sign-in and waits for it to finish.
    pub async fn login(&self) -> anyhow::Result<()> {
        self.check_is_anthropic_cli().await?;
        std::fs::create_dir_all(&self.config_dir)
            .with_context(|| format!("failed to create {}", self.config_dir.display()))?;
        let output = self
            .command()
            .args(["auth", "login", "--timeout"])
            .arg(format!("{}s", LOGIN_TIMEOUT.as_secs()))
            .output()
            .await
            .context("failed to run `ant auth login`")?;
        if !output.status.success() {
            bail!(
                "`ant auth login` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    /// A current access token for the signed-in account.
    pub async fn token(&self) -> anyhow::Result<TokenResponse> {
        let output = self
            .command()
            .args(["auth", "print-credentials"])
            .output()
            .await
            .context("failed to run `ant auth print-credentials`")?;
        if !output.status.success() {
            bail!(
                "`ant auth print-credentials` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let credentials: Credentials = serde_json::from_slice(&output.stdout)
            .context("`ant auth print-credentials` returned unexpected output")?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Ok(TokenResponse {
            access_token: credentials.access_token,
            refresh_token: None,
            expires_in: credentials
                .expires_at
                .and_then(|expires_at| i64::try_from(expires_at.saturating_sub(now)).ok()),
            token_type: Some("Bearer".to_owned()),
            scope: credentials.scope,
        })
    }

    /// Signs out of the account `ant` holds for this app.
    pub async fn logout(&self) -> anyhow::Result<()> {
        self.command()
            .args(["auth", "logout"])
            .output()
            .await
            .context("failed to run `ant auth logout`")?;
        Ok(())
    }
}
