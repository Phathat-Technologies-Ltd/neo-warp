use super::*;
use crate::agent::scripted::TextProvider;

fn settings() -> api::request::Settings {
    api::request::Settings {
        model_config: Some(api::request::settings::ModelConfig {
            base: "claude-haiku-5-5".to_owned(),
            ..Default::default()
        }),
        api_keys: Some(api::request::settings::ApiKeys {
            anthropic: "sk-ant-key".to_owned(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn search_returns_parsed_commands() {
    let provider = TextProvider::new(vec![Ok(r#"```json
[{"command": "find {{dir}} -name '*.log' -mtime +7 -delete",
  "description": "Delete logs older than a week",
  "parameters": [{"id": "dir", "description": "Directory to clean"},
                 {"id": "unused", "description": "Not in the command"}]}]
```"#)]);
    let commands = search(
        &settings(),
        "delete old log files",
        Some("macOS 15.0"),
        &provider.factory(),
    )
    .await
    .unwrap();
    assert_eq!(
        commands,
        vec![GeneratedCommand {
            command: "find {{dir}} -name '*.log' -mtime +7 -delete".to_owned(),
            description: "Delete logs older than a week".to_owned(),
            parameters: vec![Parameter {
                id: "dir".to_owned(),
                description: "Directory to clean".to_owned(),
            }],
        }]
    );
    let (_, _, user) = &provider.calls()[0];
    assert!(user.contains("macOS 15.0"));
    assert!(user.contains("delete old log files"));
}

#[tokio::test]
async fn unusable_replies_are_bad_prompts() {
    let provider = TextProvider::new(vec![Ok("I can't help with that."), Ok("[]")]);
    let factory = provider.factory();
    assert_eq!(
        search(&settings(), "make me a sandwich", None, &factory).await,
        Err(Failure::BadPrompt)
    );
    assert_eq!(
        search(&settings(), "make me a sandwich", None, &factory).await,
        Err(Failure::BadPrompt)
    );
    assert_eq!(
        search(&settings(), "   ", None, &factory).await,
        Err(Failure::BadPrompt)
    );
    assert_eq!(provider.calls().len(), 2);
}

#[tokio::test]
async fn transient_failures_are_other_and_missing_keys_are_provider_errors() {
    let provider = TextProvider::new(vec![Err("boom")]);
    assert_eq!(
        search(&settings(), "list files", None, &provider.factory()).await,
        Err(Failure::Other)
    );

    let mut no_key = settings();
    no_key.api_keys = None;
    assert_eq!(
        search(&no_key, "list files", None, &provider.factory()).await,
        Err(Failure::AiProviderError)
    );
}

#[test]
fn parse_commands_caps_and_drops_empty_commands() {
    let reply = r#"[{"command": " "}, {"command": "ls"}, {"command": "ls -a"},
        {"command": "ls -l"}, {"command": "ls -la"}]"#;
    let commands = parse_commands(reply);
    assert_eq!(
        commands
            .iter()
            .map(|command| command.command.as_str())
            .collect::<Vec<_>>(),
        vec!["ls", "ls -a", "ls -l"]
    );
}
