//! Shared plumbing for the small AI helpers (prompt suggestions, command search): one-shot model
//! calls with the agent's endpoint and credential rules, and the per-user model settings that
//! requests without their own credentials fall back to.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use oi_kit_provider_types::conversation::message::Message;
use oi_kit_provider_types::errors::ProviderError;
use oi_kit_provider_types::model::ModelConfig;
use warp_multi_agent_api as api;

use super::turn::{ProviderFactory, TurnError, classify_message_error, resolve_model};

/// Helpers answer in a sentence or a few commands; a tight budget keeps them fast and cheap.
const HELPER_MAX_OUTPUT_TOKENS: i32 = 400;

/// How long a user's last-seen model settings stay usable for requests that carry none.
const SETTINGS_TTL: Duration = Duration::from_secs(12 * 60 * 60);

/// Runs one tool-free model call with the model and credential `settings` select and returns the
/// reply's text.
pub(super) async fn complete(
    settings: &api::request::Settings,
    system: &str,
    user: String,
    make_provider: &ProviderFactory,
) -> Result<String, TurnError> {
    let (endpoint, model) = resolve_model(Some(settings))?;
    let provider = make_provider(&endpoint)
        .map_err(|error| TurnError::Internal(format!("couldn't reach the model: {error:#}")))?;
    let mut config = ModelConfig::new(&model);
    config.max_tokens = Some(HELPER_MAX_OUTPUT_TOKENS);
    let (reply, _usage) = provider
        .complete(&config, system, &[Message::user().with_text(user)], &[])
        .await
        .map_err(|error| match error {
            ProviderError::Authentication(_) => TurnError::InvalidApiKey {
                provider: endpoint.llm_provider(),
                model: model.clone(),
            },
            ProviderError::RateLimitExceeded { .. } => TurnError::Unavailable,
            other => classify_message_error(&other.to_string(), &model, &endpoint),
        })?;
    Ok(reply.as_concat_text())
}

/// The model settings from each user's latest agent or suggestion request, in memory only.
#[derive(Default)]
pub struct ModelSettingsCache {
    entries: Mutex<HashMap<String, (SystemTime, api::request::Settings)>>,
}

impl ModelSettingsCache {
    /// Remembers the model choice and credentials from `settings` for `uid`. Fields `settings`
    /// leaves empty keep their earlier value, so a request that carries only part of the settings
    /// can't erase credentials a fuller one supplied.
    pub fn record(&self, uid: &str, settings: Option<&api::request::Settings>) {
        let Some(settings) = settings else {
            return;
        };
        let mut entries = self.lock();
        let mut kept = entries
            .get(uid)
            .map(|(_, earlier)| earlier.clone())
            .unwrap_or_default();
        merge_non_empty(&mut kept.model_config, &settings.model_config);
        merge_non_empty(&mut kept.api_keys, &settings.api_keys);
        merge_non_empty(
            &mut kept.custom_model_providers,
            &settings.custom_model_providers,
        );
        if kept == api::request::Settings::default() {
            return;
        }
        entries.insert(uid.to_owned(), (SystemTime::now(), kept));
    }

    /// The settings last recorded for `uid`, unless they have expired.
    pub fn latest(&self, uid: &str) -> Option<api::request::Settings> {
        self.latest_at(uid, SystemTime::now())
    }

    fn latest_at(&self, uid: &str, now: SystemTime) -> Option<api::request::Settings> {
        let mut entries = self.lock();
        match entries.get(uid) {
            Some((recorded, settings))
                if now.duration_since(*recorded).unwrap_or_default() < SETTINGS_TTL =>
            {
                Some(settings.clone())
            }
            Some(_) => {
                entries.remove(uid);
                None
            }
            None => None,
        }
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, (SystemTime, api::request::Settings)>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn merge_non_empty<T: Clone + Default + PartialEq>(kept: &mut Option<T>, incoming: &Option<T>) {
    if let Some(value) = incoming.as_ref().filter(|value| **value != T::default()) {
        *kept = Some(value.clone());
    }
}

/// The last `max_chars` characters of `text`, marked as cut when shortened.
pub(super) fn tail(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let kept: String = text.chars().skip(count - max_chars).collect();
    format!("[...]{kept}")
}

/// The JSON value delimited by `open` and `close` in a model reply, which models often wrap in
/// prose or code fences.
pub(super) fn json_span(reply: &str, open: char, close: char) -> Option<&str> {
    let start = reply.find(open)?;
    let end = reply.rfind(close)?;
    (end > start).then(|| &reply[start..=end])
}

#[cfg(test)]
#[path = "helpers_tests.rs"]
mod tests;
