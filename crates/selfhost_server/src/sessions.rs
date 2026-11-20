//! The server's own tokens. Every token is a signed JWT carrying the user, so the server keeps no
//! session state and a sign-in survives restarts as long as the signing secret does.

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

const CUSTOM_TOKEN_LIFETIME: Duration = Duration::from_secs(5 * 60);
pub const ID_TOKEN_LIFETIME: Duration = Duration::from_secs(60 * 60);
const TOKEN_ISSUER: &str = "selfhost-server";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub uid: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
}

/// A freshly issued ID token plus refresh token.
pub struct IssuedTokens {
    pub id_token: String,
    pub refresh_token: String,
}

/// Why a token exchange was refused; the messages are the Firebase error codes the client maps
/// to "sign in again".
#[derive(Debug, PartialEq, Eq)]
pub enum TokenError {
    InvalidCustomToken,
    InvalidRefreshToken,
}

impl TokenError {
    pub fn firebase_message(&self) -> &'static str {
        match self {
            TokenError::InvalidCustomToken => "INVALID_CUSTOM_TOKEN",
            TokenError::InvalidRefreshToken => "INVALID_REFRESH_TOKEN",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TokenKind {
    /// One-time token from the provider exchange, redeemed for the first ID and refresh tokens.
    Custom,
    Refresh,
    Id,
}

#[derive(Serialize, Deserialize)]
struct TokenClaims {
    iss: String,
    kind: TokenKind,
    /// Unique per token; makes custom tokens single-use.
    jti: String,
    sub: String,
    email: Option<String>,
    name: Option<String>,
    /// When the user last signed in with the identity provider. Refresh doesn't extend it.
    signed_in_at: u64,
    iat: u64,
    exp: u64,
}

impl TokenClaims {
    fn user(&self) -> User {
        User {
            uid: self.sub.clone(),
            email: self.email.clone(),
            display_name: self.name.clone(),
        }
    }
}

pub struct SessionStore {
    signing_secret: Vec<u8>,
    max_age: Duration,
    /// Custom tokens already redeemed by this process. A restart forgets them, which only
    /// reopens tokens younger than `CUSTOM_TOKEN_LIFETIME`.
    redeemed_custom_tokens: Mutex<HashSet<String>>,
}

impl SessionStore {
    /// `signing_secret` signs every token; without one a random secret is generated, so
    /// restarting signs everyone out.
    pub fn new(signing_secret: Option<&[u8]>, max_age: Duration) -> Self {
        let signing_secret = match signing_secret {
            Some(secret) => secret.to_vec(),
            None => generate_signing_secret().to_vec(),
        };
        Self {
            signing_secret,
            max_age,
            redeemed_custom_tokens: Mutex::default(),
        }
    }

    /// Returns a one-time custom token for the provider user. The user ID is derived from the
    /// provider's `iss` and `sub`, so it is stable across sign-ins and restarts.
    pub fn sign_in(
        &self,
        issuer: &str,
        subject: &str,
        email: Option<String>,
        display_name: Option<String>,
    ) -> String {
        let user = User {
            uid: hex::encode(&Sha256::digest(format!("{issuer}|{subject}"))[..14]),
            email,
            display_name,
        };
        let now = unix_seconds(SystemTime::now());
        self.mint(
            TokenKind::Custom,
            &user,
            now,
            now + CUSTOM_TOKEN_LIFETIME.as_secs(),
        )
    }

    /// Trades a custom token, once, for an ID token and a refresh token.
    pub async fn redeem_custom_token(&self, token: &str) -> Result<IssuedTokens, TokenError> {
        let claims = self
            .decode(token, TokenKind::Custom)
            .ok_or(TokenError::InvalidCustomToken)?;
        if !self
            .redeemed_custom_tokens
            .lock()
            .await
            .insert(claims.jti.clone())
        {
            return Err(TokenError::InvalidCustomToken);
        }
        Ok(self.issue(&claims.user(), unix_seconds(SystemTime::now())))
    }

    /// Issues fresh tokens for a refresh token. Fails once the sign-in is older than the session
    /// max age, so the user has to pass the identity provider again. Earlier refresh tokens stay
    /// valid until then, since the server keeps no state to revoke them.
    pub fn refresh(&self, refresh_token: &str) -> Result<IssuedTokens, TokenError> {
        let claims = self
            .decode(refresh_token, TokenKind::Refresh)
            .ok_or(TokenError::InvalidRefreshToken)?;
        Ok(self.issue(&claims.user(), claims.signed_in_at))
    }

    /// Returns the user an ID token from this server belongs to, if it is valid.
    pub fn user_for_id_token(&self, id_token: &str) -> Option<User> {
        Some(self.decode(id_token, TokenKind::Id)?.user())
    }

    fn issue(&self, user: &User, signed_in_at: u64) -> IssuedTokens {
        let now = unix_seconds(SystemTime::now());
        let session_ends = signed_in_at + self.max_age.as_secs();
        IssuedTokens {
            id_token: self.mint(
                TokenKind::Id,
                user,
                signed_in_at,
                (now + ID_TOKEN_LIFETIME.as_secs()).min(session_ends),
            ),
            refresh_token: self.mint(TokenKind::Refresh, user, signed_in_at, session_ends),
        }
    }

    fn mint(&self, kind: TokenKind, user: &User, signed_in_at: u64, exp: u64) -> String {
        let claims = TokenClaims {
            iss: TOKEN_ISSUER.to_owned(),
            kind,
            jti: random_token(),
            sub: user.uid.clone(),
            email: user.email.clone(),
            name: user.display_name.clone(),
            signed_in_at,
            iat: unix_seconds(SystemTime::now()),
            exp,
        };
        jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(&self.signing_secret),
        )
        .expect("HS256 signing with an in-memory secret cannot fail")
    }

    fn decode(&self, token: &str, kind: TokenKind) -> Option<TokenClaims> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[TOKEN_ISSUER]);
        validation.validate_aud = false;
        validation.leeway = 0;
        let claims = jsonwebtoken::decode::<TokenClaims>(
            token,
            &DecodingKey::from_secret(&self.signing_secret),
            &validation,
        )
        .ok()?
        .claims;
        (claims.kind == kind).then_some(claims)
    }
}

/// A new random signing secret.
pub fn generate_signing_secret() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes
}

fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(generate_signing_secret())
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
