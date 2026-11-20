//! Validation of the identity provider's ID tokens against its published signing keys.

use anyhow::{Context as _, anyhow};
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::RwLock;

/// The ID token claims the server uses.
#[derive(Clone, Debug, Deserialize)]
pub struct ProviderClaims {
    pub iss: String,
    pub sub: String,
    pub email: Option<String>,
    pub preferred_username: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
}

impl ProviderClaims {
    /// The user's email, falling back to Entra's `preferred_username` (usually the UPN).
    pub fn email(&self) -> Option<&str> {
        self.email.as_deref().or(self.preferred_username.as_deref())
    }
}

/// Verifies ID tokens issued by one OIDC provider for one client.
pub struct OidcVerifier {
    /// The `issuer` from the provider's discovery document, which may differ in form from the
    /// configured issuer URL (for example, an Entra domain name versus the tenant ID).
    issuer: String,
    client_id: String,
    jwks_uri: Option<String>,
    keys: RwLock<JwkSet>,
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    jwks_uri: String,
}

impl OidcVerifier {
    /// Reads the provider's discovery document and signing keys.
    pub async fn discover(issuer: &str, client_id: &str) -> anyhow::Result<Self> {
        let url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        let discovery: DiscoveryDocument = reqwest::get(&url)
            .await
            .and_then(reqwest::Response::error_for_status)
            .with_context(|| format!("failed to fetch {url}"))?
            .json()
            .await
            .with_context(|| format!("{url} is not an OIDC discovery document"))?;
        let keys = fetch_keys(&discovery.jwks_uri).await?;
        Ok(Self {
            issuer: discovery.issuer,
            client_id: client_id.to_owned(),
            jwks_uri: Some(discovery.jwks_uri),
            keys: RwLock::new(keys),
        })
    }

    /// A verifier with fixed keys that never refetches them.
    pub fn with_keys(issuer: &str, client_id: &str, keys: JwkSet) -> Self {
        Self {
            issuer: issuer.to_owned(),
            client_id: client_id.to_owned(),
            jwks_uri: None,
            keys: RwLock::new(keys),
        }
    }

    /// Checks the token's signature, issuer, audience, and lifetime, and returns its claims.
    pub async fn verify(&self, id_token: &str) -> anyhow::Result<ProviderClaims> {
        let header = jsonwebtoken::decode_header(id_token).context("malformed ID token")?;
        let kid = header
            .kid
            .ok_or_else(|| anyhow!("the ID token has no key ID"))?;
        let key = match self.decoding_key(&kid).await? {
            Some(key) => key,
            // Providers rotate signing keys; refetch once before rejecting an unknown key ID.
            None => {
                self.refresh_keys().await?;
                self.decoding_key(&kid)
                    .await?
                    .ok_or_else(|| anyhow!("the ID token is signed with an unknown key"))?
            }
        };
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.client_id]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        Ok(
            jsonwebtoken::decode::<ProviderClaims>(id_token, &key, &validation)
                .context("the ID token failed validation")?
                .claims,
        )
    }

    async fn decoding_key(&self, kid: &str) -> anyhow::Result<Option<DecodingKey>> {
        self.keys
            .read()
            .await
            .find(kid)
            .map(|jwk| DecodingKey::from_jwk(jwk).context("unusable provider signing key"))
            .transpose()
    }

    async fn refresh_keys(&self) -> anyhow::Result<()> {
        if let Some(jwks_uri) = &self.jwks_uri {
            *self.keys.write().await = fetch_keys(jwks_uri).await?;
        }
        Ok(())
    }
}

async fn fetch_keys(jwks_uri: &str) -> anyhow::Result<JwkSet> {
    reqwest::get(jwks_uri)
        .await
        .and_then(reqwest::Response::error_for_status)
        .with_context(|| format!("failed to fetch the provider signing keys from {jwks_uri}"))?
        .json()
        .await
        .context("the provider signing keys are not a JWK set")
}
