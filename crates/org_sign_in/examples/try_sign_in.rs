//! Runs a real organization sign-in against the configured identity provider and prints the ID
//! token's claims, to check an app registration before any backend exists. With
//! `SELFHOST_SERVER_URL` set, it continues through the self-hosted server the way the client
//! does: exchange the ID token, redeem the custom token, and fetch the user.
//!
//! ```sh
//! WARP_ORG_SIGN_IN_ISSUER=https://login.microsoftonline.com/<tenant-id>/v2.0 \
//! WARP_ORG_SIGN_IN_CLIENT_ID=<client-id> \
//! SELFHOST_SERVER_URL=http://127.0.0.1:8090 \
//! cargo run -p org_sign_in --example try_sign_in
//! ```

use anyhow::Context as _;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use org_sign_in::{OrganizationSignInConfig, SignInAttempt, discover, exchange_with_backend};

fn main() -> anyhow::Result<()> {
    let config = OrganizationSignInConfig::load()?.context(
        "set WARP_ORG_SIGN_IN_ISSUER and WARP_ORG_SIGN_IN_CLIENT_ID, or deploy organization.toml",
    )?;
    futures::executor::block_on(async {
        let metadata = discover(&config.issuer).await?;
        println!("Provider configuration found:");
        println!("  authorize: {}", metadata.authorization_endpoint);
        println!("  token:     {}", metadata.token_endpoint);

        let attempt = SignInAttempt::start(&config, metadata)?;
        println!(
            "\n{}: open this URL in your browser:\n",
            config.button_label()
        );
        println!("{}\n", attempt.authorize_url());
        println!("Waiting for the browser to return (5 minutes)...");

        let (_release, result) = attempt.finish();
        let id_token = result.await?;

        let payload = id_token
            .split('.')
            .nth(1)
            .context("ID token is not a JWT")?;
        let claims: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
        println!("\nSigned in. ID token claims:");
        println!("{}", serde_json::to_string_pretty(&claims)?);

        if let Ok(server) = std::env::var("SELFHOST_SERVER_URL") {
            check_server(server.trim_end_matches('/'), &id_token).await?;
        }
        Ok(())
    })
}

async fn check_server(server: &str, id_token: &str) -> anyhow::Result<()> {
    let custom_token = exchange_with_backend(server, id_token).await?;
    println!("\nServer accepted the ID token and issued a custom token.");

    let client = http_client::Client::new_external();
    let tokens: serde_json::Value = client
        .post(format!("{server}/v1/accounts:signInWithCustomToken"))
        .form(&[
            ("returnSecureToken", "true"),
            ("token", custom_token.as_str()),
        ])
        .send()
        .await?
        .json()
        .await?;
    let server_id_token = tokens["idToken"]
        .as_str()
        .with_context(|| format!("custom token redemption failed: {tokens}"))?;
    println!("Custom token redeemed for the server's ID and refresh tokens.");

    let user: serde_json::Value = client
        .post(format!("{server}/graphql/v2?op=GetUser"))
        .bearer_auth(server_id_token)
        .json(&serde_json::json!({ "query": "", "variables": {} }))
        .send()
        .await?
        .json()
        .await?;
    println!("GetUser profile:");
    println!(
        "{}",
        serde_json::to_string_pretty(&user["data"]["user"]["user"]["profile"])?
    );
    Ok(())
}
