//! Users, one-time custom tokens, refresh tokens, and the server's own ID tokens, held in
//! memory.

use std::collections::HashMap;
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
const ID_TOKEN_ISSUER: &str = "selfhost-server";

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

struct PendingCustomToken {
    uid: String,
    expires_at: SystemTime,
}

struct Session {
    uid: String,
    /// When the user last signed in with the identity provider. Refresh doesn't extend it.
    signed_in_at: SystemTime,
}

#[derive(Serialize, Deserialize)]
struct IdTokenClaims {
    iss: String,
    sub: String,
    email: Option<String>,
    name: Option<String>,
    iat: u64,
    exp: u64,
}

#[derive(Default)]
struct State {
    users: HashMap<String, User>,
    custom_tokens: HashMap<String, PendingCustomToken>,
    sessions: HashMap<String, Session>,
}

pub struct SessionStore {
    state: Mutex<State>,
    signing_secret: Vec<u8>,
    max_age: Duration,
}

impl SessionStore {
    pub fn new(signing_secret: Option<&str>, max_age: Duration) -> Self {
        let signing_secret = match signing_secret {
            Some(secret) => secret.as_bytes().to_vec(),
            None => {
                let mut bytes = vec![0u8; 32];
                rand::thread_rng().fill_bytes(&mut bytes);
                bytes
            }
        };
        Self {
            state: Mutex::default(),
            signing_secret,
            max_age,
        }
    }

    /// Records the provider user and returns a one-time custom token for them. The user ID is
    /// derived from the provider's `iss` and `sub`, so it is stable across server restarts.
    pub async fn sign_in(
        &self,
        issuer: &str,
        subject: &str,
        email: Option<String>,
        display_name: Option<String>,
    ) -> String {
        let uid = hex::encode(&Sha256::digest(format!("{issuer}|{subject}"))[..14]);
        let custom_token = random_token();
        let mut state = self.state.lock().await;
        state.users.insert(
            uid.clone(),
            User {
                uid: uid.clone(),
                email,
                display_name,
            },
        );
        state.custom_tokens.insert(
            custom_token.clone(),
            PendingCustomToken {
                uid,
                expires_at: SystemTime::now() + CUSTOM_TOKEN_LIFETIME,
            },
        );
        custom_token
    }

    /// Trades a custom token, once, for an ID token and a refresh token.
    pub async fn redeem_custom_token(&self, token: &str) -> Result<IssuedTokens, TokenError> {
        let mut state = self.state.lock().await;
        let pending = state
            .custom_tokens
            .remove(token)
            .filter(|pending| pending.expires_at > SystemTime::now())
            .ok_or(TokenError::InvalidCustomToken)?;
        let user = state
            .users
            .get(&pending.uid)
            .cloned()
            .ok_or(TokenError::InvalidCustomToken)?;
        let refresh_token = random_token();
        state.sessions.insert(
            refresh_token.clone(),
            Session {
                uid: user.uid.clone(),
                signed_in_at: SystemTime::now(),
            },
        );
        Ok(IssuedTokens {
            id_token: self.mint_id_token(&user),
            refresh_token,
        })
    }

    /// Rotates a refresh token. Fails once the sign-in is older than the session max age, so
    /// the user has to pass the identity provider again.
    pub async fn refresh(&self, refresh_token: &str) -> Result<IssuedTokens, TokenError> {
        let mut state = self.state.lock().await;
        let session = state
            .sessions
            .remove(refresh_token)
            .ok_or(TokenError::InvalidRefreshToken)?;
        if session.signed_in_at + self.max_age <= SystemTime::now() {
            return Err(TokenError::InvalidRefreshToken);
        }
        let user = state
            .users
            .get(&session.uid)
            .cloned()
            .ok_or(TokenError::InvalidRefreshToken)?;
        let next_refresh_token = random_token();
        state.sessions.insert(next_refresh_token.clone(), session);
        Ok(IssuedTokens {
            id_token: self.mint_id_token(&user),
            refresh_token: next_refresh_token,
        })
    }

    /// Returns the user an ID token from this server belongs to, if it is valid.
    pub async fn user_for_id_token(&self, id_token: &str) -> Option<User> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[ID_TOKEN_ISSUER]);
        validation.validate_aud = false;
        let claims = jsonwebtoken::decode::<IdTokenClaims>(
            id_token,
            &DecodingKey::from_secret(&self.signing_secret),
            &validation,
        )
        .ok()?
        .claims;
        self.state.lock().await.users.get(&claims.sub).cloned()
    }

    fn mint_id_token(&self, user: &User) -> String {
        let now = unix_seconds(SystemTime::now());
        let claims = IdTokenClaims {
            iss: ID_TOKEN_ISSUER.to_owned(),
            sub: user.uid.clone(),
            email: user.email.clone(),
            name: user.display_name.clone(),
            iat: now,
            exp: now + ID_TOKEN_LIFETIME.as_secs(),
        };
        jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(&self.signing_secret),
        )
        .expect("HS256 signing with an in-memory secret cannot fail")
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
