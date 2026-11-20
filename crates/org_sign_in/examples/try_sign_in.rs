//! Runs a real organization sign-in against the configured identity provider and prints the ID
//! token's claims, to check an app registration before any backend exists.
//!
//! ```sh
//! WARP_ORG_SIGN_IN_ISSUER=https://login.microsoftonline.com/<tenant-id>/v2.0 \
//! WARP_ORG_SIGN_IN_CLIENT_ID=<client-id> \
//! cargo run -p org_sign_in --example try_sign_in
//! ```

use anyhow::Context as _;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use org_sign_in::{OrganizationSignInConfig, SignInAttempt, discover};

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
        Ok(())
    })
}
