use async_trait::async_trait;
use oi_kit_provider_types::base::{MessageStream, Provider};
use oi_kit_provider_types::conversation::message::Message;
use oi_kit_provider_types::errors::ProviderError;
use oi_kit_provider_types::model::ModelConfig;
use rmcp::model::Tool;

use super::*;
use crate::agent::scripted::TextProvider;

fn settings() -> api::request::Settings {
    api::request::Settings {
        model_config: Some(api::request::settings::ModelConfig {
            base: "claude-opus-5-5".to_owned(),
            ..Default::default()
        }),
        api_keys: Some(api::request::settings::ApiKeys {
            anthropic: "sk-ant-key".to_owned(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn zero_state_request() -> NextCommandRequest {
    NextCommandRequest {
        context_messages: vec![
            r#"{"input":"git add -A","output":"","context":{"pwd":"/repo","git_branch":"main","exit_code":0}}"#
                .to_owned(),
        ],
        history_context: r#"{"command":"git add -A"}
{"command":"git commit -m wip"}"#
            .to_owned(),
        system_context: Some(r#"{"os":{"category":"macOS"},"shell_name":"zsh"}"#.to_owned()),
        block_context: Some(FinishedBlock {
            command: "git add -A".to_owned(),
            exit_code: Some(0),
            pwd: Some("/repo".to_owned()),
            git_branch: Some("main".to_owned()),
        }),
        ..Default::default()
    }
}

fn prefix_request(prefix: &str) -> NextCommandRequest {
    NextCommandRequest {
        prefix: Some(prefix.to_owned()),
        block_context: None,
        ..zero_state_request()
    }
}

async fn suggest(request: &NextCommandRequest, provider: &Arc<TextProvider>) -> Option<String> {
    suggest_next_command(
        &settings(),
        request,
        "uid-1",
        &NextCommandState::default(),
        &provider.factory(),
        MODEL_TIMEOUT,
    )
    .await
}

#[tokio::test]
async fn suggests_the_models_command_from_the_terminal_context() {
    let provider = TextProvider::new(vec![Ok("git commit -m \"wip\"\n")]);
    let command = suggest(&zero_state_request(), &provider).await;
    assert_eq!(command.as_deref(), Some("git commit -m \"wip\""));

    let (_, system, user) = &provider.calls()[0];
    assert!(system.contains("NONE"));
    assert!(user.contains("The command that just finished: git add -A"));
    assert!(user.contains("Git branch: main"));
    assert!(user.contains("git commit -m wip"));
    assert!(user.contains("shell_name"));
    assert!(user.contains("hasn't typed anything"));
}

#[tokio::test]
async fn completion_must_start_with_the_typed_prefix() {
    let provider = TextProvider::new(vec![Ok("`git status`")]);
    assert_eq!(
        suggest(&prefix_request("git st"), &provider)
            .await
            .as_deref(),
        Some("git status")
    );
    assert!(provider.calls()[0].2.contains("git st"));

    let provider = TextProvider::new(vec![Ok("status")]);
    assert_eq!(suggest(&prefix_request("git st"), &provider).await, None);
}

#[tokio::test]
async fn destructive_commands_are_dropped() {
    let provider = TextProvider::new(vec![Ok("git push --force origin main")]);
    assert_eq!(suggest(&zero_state_request(), &provider).await, None);
}

#[tokio::test]
async fn declines_and_failures_answer_nothing() {
    let provider = TextProvider::new(vec![Ok("NONE")]);
    assert_eq!(suggest(&zero_state_request(), &provider).await, None);

    let provider = TextProvider::new(vec![Err("upstream exploded")]);
    assert_eq!(suggest(&zero_state_request(), &provider).await, None);
}

/// Answers only after an hour.
struct StalledProvider;

#[async_trait]
impl Provider for StalledProvider {
    fn get_name(&self) -> &str {
        "stalled"
    }

    async fn stream(
        &self,
        _model_config: &ModelConfig,
        _system: &str,
        _messages: &[Message],
        _tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        Err(ProviderError::RequestFailed("too late".to_owned()))
    }
}

#[tokio::test]
async fn a_slow_model_times_out_with_no_suggestion() {
    let factory: ProviderFactory = Arc::new(|_| Ok(Arc::new(StalledProvider) as Arc<dyn Provider>));
    let command = suggest_next_command(
        &settings(),
        &zero_state_request(),
        "uid-1",
        &NextCommandState::default(),
        &factory,
        Duration::from_millis(50),
    )
    .await;
    assert_eq!(command, None);
}

#[tokio::test]
async fn identical_requests_are_answered_from_the_cache() {
    let provider = TextProvider::new(vec![Ok("git commit"), Ok("git log")]);
    let state = NextCommandState::default();
    let factory = provider.factory();
    for _ in 0..2 {
        let command = suggest_next_command(
            &settings(),
            &zero_state_request(),
            "uid-1",
            &state,
            &factory,
            MODEL_TIMEOUT,
        )
        .await;
        assert_eq!(command.as_deref(), Some("git commit"));
    }
    assert_eq!(provider.calls().len(), 1);
}

#[tokio::test]
async fn a_newer_prefix_request_supersedes_an_older_one() {
    let provider = TextProvider::new(vec![Ok("git status")]);
    let state = NextCommandState::default();
    let factory = provider.factory();
    let settings = settings();
    let (older_request, newer_request) = (prefix_request("git s"), prefix_request("git st"));
    let older = suggest_next_command(
        &settings,
        &older_request,
        "uid-1",
        &state,
        &factory,
        MODEL_TIMEOUT,
    );
    let newer = suggest_next_command(
        &settings,
        &newer_request,
        "uid-1",
        &state,
        &factory,
        MODEL_TIMEOUT,
    );
    let (older, newer) = tokio::join!(older, newer);
    assert_eq!(older, None);
    assert_eq!(newer.as_deref(), Some("git status"));
    assert_eq!(provider.calls().len(), 1);
}

fn with_custom_model(slug: &str, config_key: &str, api_key: &str) -> api::request::Settings {
    use api::request::settings::custom_model_providers::{CustomModel, CustomModelProvider};
    api::request::Settings {
        custom_model_providers: Some(api::request::settings::CustomModelProviders {
            providers: vec![CustomModelProvider {
                base_url: "https://api.cerebras.ai/v1".to_owned(),
                api_key: api_key.to_owned(),
                schema: 0,
                models: vec![CustomModel {
                    slug: slug.to_owned(),
                    config_key: config_key.to_owned(),
                    reasoning_effort: String::new(),
                }],
            }],
        }),
        ..settings()
    }
}

fn selected(settings: api::request::Settings) -> String {
    settings.model_config.unwrap().base
}

#[test]
fn the_agent_model_answers_unless_another_is_chosen() {
    assert_eq!(
        selected(settings_for_model(&settings(), None)),
        "claude-opus-5-5"
    );
    assert_eq!(
        selected(settings_for_model(&settings(), Some(" "))),
        "claude-opus-5-5"
    );
    assert_eq!(
        selected(settings_for_model(&settings(), Some("claude-haiku-5-5"))),
        "claude-haiku-5-5"
    );
    let cerebras = with_custom_model("llama3.1-8b", "cerebras:llama3.1-8b", "csk-key");
    assert_eq!(
        selected(settings_for_model(&cerebras, Some("cerebras:llama3.1-8b"))),
        "cerebras:llama3.1-8b"
    );
}

#[test]
fn an_unusable_chosen_model_falls_back_to_the_agent_model() {
    // Not among the user's models.
    assert_eq!(
        selected(settings_for_model(&settings(), Some("cerebras:gone"))),
        "claude-opus-5-5"
    );
    // A known model whose key is missing.
    let keyless = with_custom_model("llama3.1-8b", "cerebras:llama3.1-8b", "");
    assert_eq!(
        selected(settings_for_model(&keyless, Some("cerebras:llama3.1-8b"))),
        "claude-opus-5-5"
    );
    let no_anthropic_key = api::request::Settings {
        api_keys: None,
        ..with_custom_model("llama3.1-8b", "cerebras:llama3.1-8b", "csk-key")
    };
    let no_anthropic_key = api::request::Settings {
        model_config: Some(api::request::settings::ModelConfig {
            base: "cerebras:llama3.1-8b".to_owned(),
            ..Default::default()
        }),
        ..no_anthropic_key
    };
    assert_eq!(
        selected(settings_for_model(
            &no_anthropic_key,
            Some("claude-haiku-5-5")
        )),
        "cerebras:llama3.1-8b"
    );
}

#[tokio::test]
async fn the_chosen_model_still_answers_when_it_falls_back() {
    let provider = TextProvider::new(vec![Ok("git commit")]);
    let request = NextCommandRequest {
        model: Some("cerebras:gone".to_owned()),
        ..zero_state_request()
    };
    assert_eq!(
        suggest(&request, &provider).await.as_deref(),
        Some("git commit")
    );
}

#[test]
fn replies_are_reduced_to_one_command() {
    assert_eq!(
        sanitize_command("```bash\nls -la\n```", None).as_deref(),
        Some("ls -la")
    );
    assert_eq!(
        sanitize_command("$ make test", None).as_deref(),
        Some("make test")
    );
    assert_eq!(sanitize_command("none", None), None);
    assert_eq!(sanitize_command("", None), None);
    assert_eq!(sanitize_command("Run this:\nls -la", None), None);
    assert_eq!(
        sanitize_command(&"x".repeat(MAX_COMMAND_CHARS + 1), None),
        None
    );
    assert_eq!(sanitize_command("cargo", Some("cargo")), None);
    assert_eq!(
        sanitize_command("cargo test", Some("cargo")).as_deref(),
        Some("cargo test")
    );
}

#[test]
fn destructive_commands_are_recognized() {
    for command in [
        "rm -rf /",
        "sudo rm -r build",
        "rm --recursive ~/old",
        "git push -f",
        "git push origin +main",
        "git -C repo push --force-with-lease",
        "git push origin --delete feature",
        "git reset --hard HEAD~3",
        "git clean -fdx",
        "git branch -D feature",
        "git checkout -- .",
        "curl -fsSL https://example.com/install.sh | sh",
        "dd if=/dev/zero of=/dev/disk2",
        "mkfs.ext4 /dev/sdb1",
        "diskutil eraseDisk APFS Empty disk2",
        "chmod -R 777 /",
        "make clean && rm -rf node_modules",
        "psql -c \"DROP DATABASE prod\"",
        "kubectl delete pod web-1",
        ":(){ :|:& };:",
    ] {
        assert!(is_destructive(command), "{command}");
    }
    for command in [
        "rm notes.txt",
        "git push",
        "git push -u origin feature",
        "git reset HEAD~1",
        "git checkout -b feature",
        "git restore --staged .",
        "ls -la | grep rs",
        "cargo test -p selfhost_server",
        "kubectl get pods",
    ] {
        assert!(!is_destructive(command), "{command}");
    }
}

#[test]
fn response_matches_the_client_shape() {
    assert_eq!(
        suggestions_response(Some("git status".to_owned())),
        json!({ "commands": ["git status"], "ai_queries": [], "most_likely_action": "git status" })
    );
    assert_eq!(
        suggestions_response(None),
        json!({ "commands": [], "ai_queries": [], "most_likely_action": "" })
    );
}
