//! `selfhost-server [config.toml]`: runs the self-hosted sign-in server. The config path
//! defaults to `selfhost-server.toml`, or `SELFHOST_SERVER_CONFIG` when set.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use selfhost_server::config::ServerConfig;
use selfhost_server::{AppState, router};

const CONFIG_ENV: &str = "SELFHOST_SERVER_CONFIG";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let config_path = std::env::args_os()
        .nth(1)
        .or_else(|| std::env::var_os(CONFIG_ENV))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("selfhost-server.toml"));
    let config = ServerConfig::from_file(&config_path)?;
    if config.session.signing_secret.is_none() {
        log::warn!(
            "No session.signing_secret configured; restarting the server signs everyone out"
        );
    }

    let state = Arc::new(AppState::from_config(&config));
    let listener = tokio::net::TcpListener::bind(&config.listen)
        .await
        .with_context(|| format!("failed to listen on {}", config.listen))?;
    log::info!(
        "Self-hosted server listening on http://{} for issuer {}",
        config.listen,
        config.oidc.issuer
    );
    axum::serve(listener, router(state))
        .await
        .context("server stopped")
}
