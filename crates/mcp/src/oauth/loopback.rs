use std::time::Duration;

use oauth_loopback::{AuthorizationDenied, CancellationHandle, LoopbackConfig, LoopbackListener};
use rmcp::transport::AuthError;

use super::CallbackResult;

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);
const SUCCESS_HTML: &str = "<!doctype html><html><body><p>Authentication complete. You can close \
     this tab and return to neo-warp.</p></body></html>";
const FAILURE_HTML: &str = "<!doctype html><html><body><p>Authentication failed. Return to \
     neo-warp for details.</p></body></html>";

/// Host written into the loopback redirect URI (`http://<host>:<port>/callback`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoopbackRedirectHost {
    /// `127.0.0.1`, the RFC 8252 §7.3 recommendation.
    #[default]
    Ipv4Literal,
    /// `localhost`, for gateways that only allow that spelling.
    Localhost,
}

impl LoopbackRedirectHost {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ipv4Literal => "127.0.0.1",
            Self::Localhost => "localhost",
        }
    }
}

/// Where the loopback callback listener binds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoopbackSettings {
    /// Fixed port, or `None` for an OS-assigned one.
    pub port: Option<u16>,
    pub host: LoopbackRedirectHost,
}

impl LoopbackSettings {
    fn config(self) -> LoopbackConfig {
        LoopbackConfig {
            label: "MCP",
            port: self.port.unwrap_or(0),
            redirect_host: self.host.as_str(),
            timeout: CALLBACK_TIMEOUT,
            cors_allowed_origins: &[],
            success_html: SUCCESS_HTML,
            failure_html: FAILURE_HTML,
        }
    }
}

pub struct LoopbackOAuthReceiver {
    listener: LoopbackListener,
    redirect_uri: String,
}

/// Stops the callback thread (and frees the port) when the OAuth flow is abandoned.
struct CancelOnDrop(CancellationHandle);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl LoopbackOAuthReceiver {
    pub fn bind(settings: LoopbackSettings) -> anyhow::Result<Self> {
        let listener = LoopbackListener::bind(settings.config())?;
        let redirect_uri = listener.redirect_uri();
        Ok(Self {
            listener,
            redirect_uri,
        })
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    pub async fn receive(self, expected_state: &str) -> Result<CallbackResult, AuthError> {
        let cancellation = CancelOnDrop(CancellationHandle::new());
        let (_release, callback) = self
            .listener
            .wait_for_callback(expected_state.to_owned(), cancellation.0.clone());
        match callback.await {
            Ok(callback) => Ok(CallbackResult::Success {
                code: callback.code,
                csrf_token: expected_state.to_owned(),
                issuer: callback.issuer,
            }),
            Err(err) => match err.downcast::<AuthorizationDenied>() {
                Ok(denied) => Ok(CallbackResult::Error {
                    error: Some(denied.description.unwrap_or(denied.error)),
                }),
                Err(err) => Err(AuthError::AuthorizationFailed(format!("{err:#}"))),
            },
        }
    }
}
