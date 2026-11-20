//! Validation of the identity provider's ID tokens against its published signing keys.

use anyhow::{Context as _, anyhow};
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::{OnceCell, RwLock};

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

/// Verifies ID tokens issued by one OIDC provider for one client. The provider's discovery
/// document and signing keys are fetched on first use, so creating a verifier needs no network.
pub struct OidcVerifier {
    configured_issuer: String,
    client_id: String,
    discovered: OnceCell<Discovered>,
    keys: RwLock<JwkSet>,
}

struct Discovered {
    /// The `issuer` from the provider's discovery document, which may differ in form from the
    /// configured issuer URL (for example, an Entra domain name versus the tenant ID).
    issuer: String,
    jwks_uri: Option<String>,
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    jwks_uri: String,
}

impl OidcVerifier {
    pub fn new(issuer: &str, client_id: &str) -> Self {
        Self {
            configured_issuer: issuer.to_owned(),
            client_id: client_id.to_owned(),
            discovered: OnceCell::new(),
            keys: RwLock::new(JwkSet { keys: Vec::new() }),
        }
    }

    /// A verifier with fixed keys that never contacts the provider.
    pub fn with_keys(issuer: &str, client_id: &str, keys: JwkSet) -> Self {
        Self {
            configured_issuer: issuer.to_owned(),
            client_id: client_id.to_owned(),
            discovered: OnceCell::new_with(Some(Discovered {
                issuer: issuer.to_owned(),
                jwks_uri: None,
            })),
            keys: RwLock::new(keys),
        }
    }

    async fn discovered(&self) -> anyhow::Result<&Discovered> {
        self.discovered
            .get_or_try_init(|| async {
                let url = format!(
                    "{}/.well-known/openid-configuration",
                    self.configured_issuer.trim_end_matches('/')
                );
                let document: DiscoveryDocument = reqwest::get(&url)
                    .await
                    .and_then(reqwest::Response::error_for_status)
                    .with_context(|| format!("failed to fetch {url}"))?
                    .json()
                    .await
                    .with_context(|| format!("{url} is not an OIDC discovery document"))?;
                Ok(Discovered {
                    issuer: document.issuer,
                    jwks_uri: Some(document.jwks_uri),
                })
            })
            .await
    }

    /// Checks the token's signature, issuer, audience, and lifetime, and returns its claims.
    pub async fn verify(&self, id_token: &str) -> anyhow::Result<ProviderClaims> {
        let header = jsonwebtoken::decode_header(id_token).context("malformed ID token")?;
        let kid = header
            .kid
            .ok_or_else(|| anyhow!("the ID token has no key ID"))?;
        let discovered = self.discovered().await?;
        let key = match self.decoding_key(&kid).await? {
            Some(key) => key,
            // Keys are fetched lazily and providers rotate them; fetch once before rejecting an
            // unknown key ID.
            None => {
                self.refresh_keys(discovered).await?;
                self.decoding_key(&kid)
                    .await?
                    .ok_or_else(|| anyhow!("the ID token is signed with an unknown key"))?
            }
        };
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&discovered.issuer]);
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

    async fn refresh_keys(&self, discovered: &Discovered) -> anyhow::Result<()> {
        if let Some(jwks_uri) = &discovered.jwks_uri {
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
