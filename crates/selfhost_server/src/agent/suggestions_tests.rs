use api::request::input::generate_passive_suggestions::{
    AgentResponseCompleted, ShellCommandCompleted,
};

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

fn passive_request(trigger: Trigger, tasks: Vec<api::Task>) -> api::Request {
    api::Request {
        task_context: Some(api::request::TaskContext { tasks }),
        input: Some(api::request::Input {
            context: None,
            r#type: Some(api::request::input::Type::GeneratePassiveSuggestions(
                api::request::input::GeneratePassiveSuggestions {
                    attachments: vec![],
                    trigger: Some(trigger),
                },
            )),
        }),
        settings: Some(settings()),
        ..Default::default()
    }
}

fn shell_trigger(command: &str, exit_code: i32, output: &str) -> Trigger {
    Trigger::ShellCommandCompleted(ShellCommandCompleted {
        executed_shell_command: Some(api::ExecutedShellCommand {
            command: command.to_owned(),
            output: output.to_owned(),
            exit_code,
            ..Default::default()
        }),
        relevant_files: vec![],
    })
}

async fn run(
    request: api::Request,
    provider: &Arc<TextProvider>,
) -> Vec<api::response_event::Type> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    suggest(request, tx, provider.factory()).await;
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event.r#type.unwrap());
    }
    events
}

fn chip(events: &[api::response_event::Type]) -> Option<PromptChip> {
    events.iter().find_map(|event| {
        let api::response_event::Type::ClientActions(actions) = event else {
            return None;
        };
        actions.actions.iter().find_map(|action| {
            let Some(client_action::Action::CreateTask(create)) = &action.action else {
                return None;
            };
            let message = create.task.as_ref()?.messages.first()?;
            let Some(Kind::ToolCall(ToolCall {
                tool: Some(Tool::SuggestPrompt(suggest)),
                ..
            })) = &message.message
            else {
                return None;
            };
            match &suggest.display_mode {
                Some(DisplayMode::PromptChip(chip)) => Some(chip.clone()),
                _ => None,
            }
        })
    })
}

fn assert_framed(events: &[api::response_event::Type]) {
    assert!(matches!(
        events.first(),
        Some(api::response_event::Type::Init(_))
    ));
    assert!(matches!(
        events.last(),
        Some(api::response_event::Type::Finished(
            api::response_event::StreamFinished {
                reason: Some(api::response_event::stream_finished::Reason::Done(_)),
                ..
            }
        ))
    ));
}

#[tokio::test]
async fn failed_command_gets_a_prompt_chip() {
    let provider = TextProvider::new(vec![Ok(
        "```json\n{\"prompt\": \"Fix the failing test in auth_tests.rs\", \"label\": \"Fix test\"}\n```",
    )]);
    let events = run(
        passive_request(
            shell_trigger("cargo test", 101, "test auth::login ... FAILED"),
            vec![],
        ),
        &provider,
    )
    .await;

    assert_framed(&events);
    let chip = chip(&events).expect("a prompt chip");
    assert_eq!(chip.prompt, "Fix the failing test in auth_tests.rs");
    assert_eq!(chip.label, "Fix test");
    let (_, _, user) = &provider.calls()[0];
    assert!(user.contains("cargo test"));
    assert!(user.contains("Exit code: 101"));
    assert!(user.contains("FAILED"));
}

#[tokio::test]
async fn declined_suggestion_sends_no_chip() {
    let provider = TextProvider::new(vec![Ok("NONE")]);
    let events = run(
        passive_request(shell_trigger("ls", 0, "README.md"), vec![]),
        &provider,
    )
    .await;
    assert_framed(&events);
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn deprecated_triggers_skip_the_model() {
    let provider = TextProvider::new(vec![]);
    let events = run(passive_request(Trigger::CommandRun(()), vec![]), &provider).await;
    assert_eq!(events.len(), 2);
    assert!(provider.calls().is_empty());
}

#[tokio::test]
async fn model_errors_end_quietly() {
    let provider = TextProvider::new(vec![Err("boom")]);
    let events = run(
        passive_request(shell_trigger("make", 2, "error"), vec![]),
        &provider,
    )
    .await;
    assert_framed(&events);
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn agent_response_trigger_uses_the_last_exchange() {
    let message = |kind| api::Message {
        message: Some(kind),
        ..Default::default()
    };
    let task = api::Task {
        id: "root".to_owned(),
        messages: vec![
            message(Kind::UserQuery(api::message::UserQuery {
                query: "add a login page".to_owned(),
                ..Default::default()
            })),
            message(Kind::AgentOutput(api::message::AgentOutput {
                text: "I added the page; tests aren't written yet.".to_owned(),
            })),
        ],
        ..Default::default()
    };
    let provider = TextProvider::new(vec![Ok("{\"prompt\": \"Write tests for the login page\"}")]);
    let events = run(
        passive_request(
            Trigger::AgentResponseCompleted(AgentResponseCompleted {}),
            vec![task],
        ),
        &provider,
    )
    .await;
    assert_eq!(
        chip(&events).unwrap().prompt,
        "Write tests for the login page"
    );
    let (_, _, user) = &provider.calls()[0];
    assert!(user.contains("add a login page"));
    assert!(user.contains("tests aren't written yet"));
}

#[test]
fn parse_suggestion_rejects_unusable_replies() {
    assert_eq!(parse_suggestion("NONE"), None);
    assert_eq!(parse_suggestion("{\"prompt\": \"  \"}"), None);
    assert_eq!(
        parse_suggestion(&format!(
            "{{\"prompt\": \"{}\"}}",
            "a".repeat(MAX_PROMPT_CHARS + 1)
        )),
        None
    );
    assert_eq!(
        parse_suggestion("Sure: {\"prompt\": \"Explain the error\"}"),
        Some(Suggestion {
            prompt: "Explain the error".to_owned(),
            label: String::new(),
        })
    );
}

#[tokio::test]
async fn legacy_suggestion_uses_the_context_messages() {
    let provider = TextProvider::new(vec![Ok("{\"prompt\": \"Explain the segfault\"}")]);
    let request = LegacySuggestionRequest {
        context_messages: vec!["./a.out\nSegmentation fault".to_owned()],
        system_context: Some("macOS".to_owned()),
        exit_code: 139,
    };
    let suggestion = legacy_suggestion(&settings(), &request, &provider.factory()).await;
    assert_eq!(suggestion.unwrap().prompt, "Explain the segfault");
    let (_, _, user) = &provider.calls()[0];
    assert!(user.contains("Segmentation fault"));
    assert!(user.contains("139"));
    assert!(user.contains("macOS"));
}
