use super::*;
use crate::agent::scripted::TextProvider;

fn settings(model: &str, anthropic_key: &str) -> api::request::Settings {
    api::request::Settings {
        model_config: Some(api::request::settings::ModelConfig {
            base: model.to_owned(),
            ..Default::default()
        }),
        api_keys: Some(api::request::settings::ApiKeys {
            anthropic: anthropic_key.to_owned(),
            ..Default::default()
        }),
        rules_enabled: true,
        ..Default::default()
    }
}

#[test]
fn cache_keeps_only_model_and_credentials_per_user() {
    let cache = ModelSettingsCache::default();
    cache.record("ada", Some(&settings("claude-haiku-5-5", "sk-ant-ada")));
    cache.record("bob", Some(&settings("claude-opus-5-5", "sk-ant-bob")));
    cache.record("ada", None);

    let ada = cache.latest("ada").unwrap();
    assert_eq!(ada.model_config.unwrap().base, "claude-haiku-5-5");
    assert_eq!(ada.api_keys.unwrap().anthropic, "sk-ant-ada");
    assert!(!ada.rules_enabled);
    assert_eq!(
        cache.latest("bob").unwrap().api_keys.unwrap().anthropic,
        "sk-ant-bob"
    );
    assert!(cache.latest("carol").is_none());
}

#[test]
fn cache_forgets_expired_settings() {
    let cache = ModelSettingsCache::default();
    cache.record("ada", Some(&settings("claude-haiku-5-5", "sk-ant-ada")));
    let later = SystemTime::now() + SETTINGS_TTL + Duration::from_secs(1);
    assert!(cache.latest_at("ada", later).is_none());
    assert!(cache.latest("ada").is_none());
}

#[tokio::test]
async fn complete_uses_a_small_output_budget_and_returns_text() {
    let provider = TextProvider::new(vec![Ok("hello")]);
    let reply = complete(
        &settings("claude-haiku-5-5", "sk-ant-key"),
        "system",
        "user text".to_owned(),
        &provider.factory(),
    )
    .await;
    assert!(matches!(reply, Ok(text) if text == "hello"));
    assert_eq!(
        provider.calls(),
        vec![(
            Some(HELPER_MAX_OUTPUT_TOKENS),
            "system".to_owned(),
            "user text".to_owned()
        )]
    );
}

#[tokio::test]
async fn complete_without_a_credential_is_an_invalid_key() {
    let provider = TextProvider::new(vec![]);
    let reply = complete(
        &settings("claude-haiku-5-5", " "),
        "system",
        "user".to_owned(),
        &provider.factory(),
    )
    .await;
    assert!(matches!(reply, Err(TurnError::InvalidApiKey { .. })));
    assert!(provider.calls().is_empty());
}

#[test]
fn tail_keeps_the_end_of_long_text() {
    assert_eq!(tail("short", 10), "short");
    assert_eq!(tail("abcdef", 3), "[...]def");
}

#[test]
fn json_span_finds_wrapped_json() {
    assert_eq!(
        json_span("```json\n{\"a\": 1}\n```", '{', '}'),
        Some("{\"a\": 1}")
    );
    assert_eq!(json_span("NONE", '{', '}'), None);
}
