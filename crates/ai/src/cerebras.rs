//! Cerebras as a built-in provider. The user pastes only an API key; the models it may use come
//! from Cerebras's OpenAI-compatible API, and requests reach Cerebras as a preset custom endpoint.

use serde::Deserialize;

use crate::api_keys::{CustomEndpoint, CustomEndpointModel, CustomEndpointSchema};

pub const BASE_URL: &str = "https://api.cerebras.ai/v1";
pub const DISPLAY_NAME: &str = "Cerebras";

/// Prefix of Cerebras models' `config_key`s, keeping them apart from the UUIDs of user-defined
/// custom endpoint models.
const CONFIG_KEY_PREFIX: &str = "cerebras:";

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// The custom endpoint that carries Cerebras requests, offering `model_ids`.
pub(crate) fn endpoint(api_key: &str, model_ids: &[String]) -> CustomEndpoint {
    CustomEndpoint {
        name: DISPLAY_NAME.to_owned(),
        url: BASE_URL.to_owned(),
        api_key: api_key.to_owned(),
        models: model_ids
            .iter()
            .map(|id| CustomEndpointModel {
                name: id.clone(),
                alias: None,
                config_key: format!("{CONFIG_KEY_PREFIX}{id}"),
            })
            .collect(),
        schema: CustomEndpointSchema::OpenaiChatCompletions,
    }
}

/// Whether `config_key` names a Cerebras model.
pub fn is_cerebras_config_key(config_key: &str) -> bool {
    config_key.starts_with(CONFIG_KEY_PREFIX)
}

/// The model IDs in a `GET /models` response body, sorted.
fn parse_models(body: &str) -> anyhow::Result<Vec<String>> {
    let list: ModelList = serde_json::from_str(body)?;
    let mut ids: Vec<_> = list
        .data
        .into_iter()
        .map(|model| model.id)
        .filter(|id| !id.trim().is_empty())
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Lists the models `api_key` may use.
#[cfg(not(target_family = "wasm"))]
pub async fn fetch_models(api_key: String) -> anyhow::Result<Vec<String>> {
    use anyhow::{Context, bail};

    let response = http_client::Client::new()
        .get(format!("{BASE_URL}/models"))
        .bearer_auth(api_key)
        .send()
        .await
        .context("failed to reach Cerebras")?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("Cerebras refused to list models ({status}): {body}");
    }
    parse_models(&body).context("Cerebras returned an unexpected model list")
}

#[cfg(test)]
#[path = "cerebras_tests.rs"]
mod tests;
