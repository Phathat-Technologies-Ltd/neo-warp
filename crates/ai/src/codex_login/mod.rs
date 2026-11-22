//! ChatGPT plan login. The user approves a device code in the browser; requests then reach
//! ChatGPT's Codex backend as a preset OpenAI Responses endpoint.
//!
//! Unsupported stopgap: this borrows the Codex CLI's OAuth client and calls
//! `chatgpt.com/backend-api/codex`, which OpenAI doesn't document for other apps. Move to "Sign in
//! with ChatGPT" plan usage (the app's own dynamically registered client, a browser PKCE login, and
//! `api.openai.com/v1/responses`) once OpenAI enables it for the workspace; see
//! <https://developers.openai.com/siwc/token-sharing-open-source>. Workspaces that don't allow
//! third-party apps refuse that route with `3p_delegated_access_policy_denied`.

#[cfg(not(target_family = "wasm"))]
pub mod oauth;

use crate::api_keys::{CodexTokens, CustomEndpoint, CustomEndpointModel, CustomEndpointSchema};

pub const DISPLAY_NAME: &str = "ChatGPT";
const CODEX_API_URL: &str = "https://chatgpt.com/backend-api/codex";

/// Prefix of ChatGPT models' `config_key`s, keeping them apart from other custom models.
const CONFIG_KEY_PREFIX: &str = "chatgpt:";

/// The endpoint that carries ChatGPT plan requests, offering `model_slugs`.
pub(crate) fn endpoint(access_token: &str, model_slugs: &[String]) -> CustomEndpoint {
    CustomEndpoint {
        name: DISPLAY_NAME.to_owned(),
        url: CODEX_API_URL.to_owned(),
        api_key: access_token.to_owned(),
        models: model_slugs
            .iter()
            .map(|slug| CustomEndpointModel {
                name: slug.clone(),
                alias: None,
                config_key: format!("{CONFIG_KEY_PREFIX}{slug}"),
            })
            .collect(),
        schema: CustomEndpointSchema::OpenaiResponses,
    }
}

/// Whether two logins' tokens belong to the same ChatGPT account.
pub(crate) fn same_account(a: &CodexTokens, b: &CodexTokens) -> bool {
    #[cfg(not(target_family = "wasm"))]
    {
        oauth::account_id(&a.access_token) == oauth::account_id(&b.access_token)
    }
    #[cfg(target_family = "wasm")]
    {
        a.access_token == b.access_token
    }
}

/// Whether `config_key` names a ChatGPT plan model.
pub fn is_chatgpt_config_key(config_key: &str) -> bool {
    config_key.starts_with(CONFIG_KEY_PREFIX)
}

#[cfg(not(target_family = "wasm"))]
mod refresh {
    use std::time::{Duration, SystemTime};

    use warpui_core::ModelContext;
    use warpui_core::r#async::Timer;

    use super::oauth::{self, TokenResponse};
    use crate::api_keys::{ApiKeyManager, CodexTokens};

    /// Access tokens last ten days; renew a day early so a sleeping laptop rarely misses it.
    const REFRESH_LEAD_TIME: Duration = Duration::from_secs(24 * 60 * 60);

    impl ApiKeyManager {
        /// Stores the tokens from a finished login or refresh, keeping the previous refresh
        /// token when the response doesn't rotate it.
        pub fn store_codex_tokens(
            &mut self,
            response: TokenResponse,
            ctx: &mut ModelContext<Self>,
        ) {
            let refresh_token = response
                .refresh_token
                .or_else(|| self.codex_tokens().and_then(|t| t.refresh_token.clone()));
            let expires_at = response
                .expires_in
                .and_then(|seconds| SystemTime::now().checked_add(Duration::from_secs(seconds)));
            self.set_codex_tokens(
                Some(CodexTokens {
                    access_token: response.access_token,
                    refresh_token,
                    expires_at,
                }),
                ctx,
            );
            self.schedule_codex_refresh(ctx);
        }

        pub fn disconnect_codex_login(&mut self, ctx: &mut ModelContext<Self>) {
            self.set_codex_tokens(None, ctx);
        }

        /// Renews the access token shortly before it expires. Tokens replaced or removed in the
        /// meantime cancel the renewal.
        pub(crate) fn schedule_codex_refresh(&mut self, ctx: &mut ModelContext<Self>) {
            let Some(tokens) = self.codex_tokens().cloned() else {
                return;
            };
            let (Some(refresh_token), Some(expires_at)) = (tokens.refresh_token, tokens.expires_at)
            else {
                return;
            };
            let now = SystemTime::now();
            let delay = expires_at
                .checked_sub(REFRESH_LEAD_TIME)
                .and_then(|fire_at| fire_at.duration_since(now).ok())
                .unwrap_or(Duration::ZERO);
            let access_token = tokens.access_token;
            ctx.spawn(
                async move {
                    Timer::after(delay).await;
                    oauth::refresh_access_token(&refresh_token).await
                },
                move |manager, result, ctx| {
                    if manager
                        .codex_tokens()
                        .is_none_or(|current| current.access_token != access_token)
                    {
                        return;
                    }
                    match result {
                        Ok(response) => manager.store_codex_tokens(response, ctx),
                        Err(error) => log::warn!("Couldn't renew the ChatGPT login: {error:#}"),
                    }
                },
            );
        }
    }
}
