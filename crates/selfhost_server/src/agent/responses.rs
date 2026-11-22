//! A model provider for endpoints that speak the OpenAI Responses API, including ChatGPT's Codex
//! backend, which a ChatGPT plan login reaches.
//!
//! Unsupported stopgap: the ChatGPT login borrows the Codex CLI's OAuth client and calls
//! `chatgpt.com/backend-api/codex`, which OpenAI doesn't document for other apps. Move to "Sign in
//! with ChatGPT" plan usage (`api.openai.com/v1/responses` with the app's own registered client)
//! once OpenAI enables it for the workspace; see
//! <https://developers.openai.com/siwc/token-sharing-open-source>.

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use oi_kit_provider_types::base::{MessageStream, Provider};
use oi_kit_provider_types::conversation::message::Message;
use oi_kit_provider_types::errors::ProviderError;
use oi_kit_provider_types::formats::openai_responses::create_responses_request_for_model;
use oi_kit_provider_types::model::ModelConfig;
use oi_kit_providers::api_client::{ApiClient, AuthMethod};
use oi_kit_providers::http_status::handle_status;
use oi_kit_providers::openai_compatible::stream_responses_compat;
use rmcp::model::Tool;
use serde_json::{Value, json};

/// The host of ChatGPT's Codex backend.
const CODEX_BACKEND_HOST: &str = "chatgpt.com";

/// Request fields the ChatGPT backend rejects; the Responses builder may add them.
const UNSUPPORTED_FIELDS: &[&str] = &[
    "max_output_tokens",
    "metadata",
    "prompt_cache_retention",
    "safety_identifier",
    "temperature",
    "top_p",
    "truncation",
    "user",
];

pub struct ResponsesProvider {
    name: String,
    api_client: ApiClient,
    codex_backend: bool,
}

impl ResponsesProvider {
    pub fn new(name: String, base_url: &str, access_token: &str) -> Result<Self> {
        let codex_backend = is_codex_backend(base_url);
        let mut api_client = ApiClient::new_with_tls(
            base_url.to_owned(),
            AuthMethod::BearerToken(access_token.to_owned()),
            None,
        )?;
        if codex_backend && let Some(account_id) = chatgpt_account_id(access_token) {
            api_client = api_client.with_header("chatgpt-account-id", &account_id)?;
        }
        Ok(Self {
            name,
            api_client,
            codex_backend,
        })
    }
}

#[async_trait]
impl Provider for ResponsesProvider {
    fn get_name(&self) -> &str {
        &self.name
    }

    async fn stream(
        &self,
        model_config: &ModelConfig,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        let model = model_config.model_name.as_str();
        let mut payload =
            create_responses_request_for_model(model_config, model, model, system, messages, tools)
                .map_err(|error| ProviderError::ExecutionError(format!("{error:#}")))?;
        shape_payload(&mut payload, system, !tools.is_empty(), self.codex_backend);
        let response = self
            .api_client
            .request("responses")
            .streaming(true)
            .response_post(&payload)
            .await
            .map_err(|error| ProviderError::RequestFailed(format!("{error:#}")))?;
        stream_responses_compat(handle_status(response).await?, None)
    }
}

/// Fits a Responses request to what ChatGPT plan inference accepts: the system prompt as
/// `instructions` rather than a system message, nothing stored server-side, streaming, and none
/// of the sampling or limit fields it rejects. The Codex backend also gets Codex's reasoning
/// defaults, so reasoning state can be replayed without server-side storage.
fn shape_payload(payload: &mut Value, system: &str, has_tools: bool, codex_backend: bool) {
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    if let Some(Value::Array(input)) = object.get_mut("input") {
        input.retain(|item| item.get("role").and_then(Value::as_str) != Some("system"));
    }
    if !system.is_empty() {
        object.insert("instructions".to_owned(), json!(system));
    }
    object.insert("store".to_owned(), json!(false));
    object.insert("stream".to_owned(), json!(true));
    for field in UNSUPPORTED_FIELDS {
        object.remove(*field);
    }
    if codex_backend {
        object.insert("include".to_owned(), json!(["reasoning.encrypted_content"]));
        object
            .entry("reasoning")
            .or_insert_with(|| json!({ "effort": "medium", "summary": "auto" }));
        if has_tools {
            object.insert("tool_choice".to_owned(), json!("auto"));
            object.insert("parallel_tool_calls".to_owned(), json!(true));
        }
    }
}

pub fn is_codex_backend(base_url: &str) -> bool {
    url::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(|host| host == CODEX_BACKEND_HOST))
        .unwrap_or(false)
}

/// The ChatGPT account (workspace) an access token was issued for, from its unverified claims.
/// The backend checks the token itself; this only names the account it should bill.
fn chatgpt_account_id(access_token: &str) -> Option<String> {
    let payload = access_token.split('.').nth(1)?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    claims
        .get("chatgpt_account_id")
        .or_else(|| claims.pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "responses_tests.rs"]
mod tests;
