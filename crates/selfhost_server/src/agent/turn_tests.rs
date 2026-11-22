use std::collections::VecDeque;

use oi_kit_provider_types::base::MessageStream;
use oi_kit_provider_types::errors::ProviderError;
use rmcp::model::CallToolRequestParams;
use serde_json::json;

use super::*;

/// What one model call saw: the system prompt, the conversation, and the offered tool names.
type RecordedCall = (String, Vec<Message>, Vec<String>);

/// Replies with one scripted list of chunks per model call and records what each call saw.
struct ScriptedProvider {
    replies: Mutex<VecDeque<Vec<Message>>>,
    calls: Mutex<Vec<RecordedCall>>,
}

impl ScriptedProvider {
    fn new(replies: Vec<Vec<Message>>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            calls: Mutex::default(),
        })
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn get_name(&self) -> &str {
        "scripted"
    }

    async fn stream(
        &self,
        _model_config: &ModelConfig,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        self.calls.lock().unwrap().push((
            system.to_owned(),
            messages.to_vec(),
            tools.iter().map(|tool| tool.name.to_string()).collect(),
        ));
        let reply = self.replies.lock().unwrap().pop_front().unwrap_or_default();
        Ok(Box::pin(futures::stream::iter(
            reply.into_iter().map(|message| Ok((Some(message), None))),
        )))
    }
}

fn request(input: api::request::input::Type, tasks: Vec<api::Task>) -> api::Request {
    api::Request {
        task_context: Some(api::request::TaskContext { tasks }),
        input: Some(api::request::Input {
            context: None,
            r#type: Some(input),
        }),
        settings: Some(api::request::Settings {
            model_config: Some(api::request::settings::ModelConfig {
                base: "claude-haiku-5-5".to_owned(),
                ..Default::default()
            }),
            api_keys: Some(api::request::settings::ApiKeys {
                anthropic: "sk-ant-api-test".to_owned(),
                ..Default::default()
            }),
            supported_tools: vec![
                api::ToolType::RunShellCommand as i32,
                api::ToolType::ReadFiles as i32,
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn user_query(text: &str) -> api::request::input::Type {
    use api::request::input::user_inputs::{UserInput, user_input};
    api::request::input::Type::UserInputs(api::request::input::UserInputs {
        inputs: vec![UserInput {
            input: Some(user_input::Input::UserQuery(
                api::request::input::UserQuery {
                    query: text.to_owned(),
                    ..Default::default()
                },
            )),
        }],
    })
}

fn shell_result(tool_call_id: &str, output: &str, exit_code: i32) -> api::request::input::Type {
    use api::request::input::user_inputs::{UserInput, user_input};
    api::request::input::Type::UserInputs(api::request::input::UserInputs {
        inputs: vec![UserInput {
            input: Some(user_input::Input::ToolCallResult(
                api::request::input::ToolCallResult {
                    tool_call_id: tool_call_id.to_owned(),
                    result: Some(
                        api::request::input::tool_call_result::Result::RunShellCommand(
                            api::RunShellCommandResult {
                                command: "ls".to_owned(),
                                result: Some(
                                    api::run_shell_command_result::Result::CommandFinished(
                                        api::ShellCommandFinished {
                                            output: output.to_owned(),
                                            exit_code,
                                            ..Default::default()
                                        },
                                    ),
                                ),
                                ..Default::default()
                            },
                        ),
                    ),
                },
            )),
        }],
    })
}

async fn run_turn(
    request: api::Request,
    provider: Arc<ScriptedProvider>,
) -> Vec<api::response_event::Type> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let factory: ProviderFactory = Arc::new(move |_| Ok(provider.clone() as Arc<dyn Provider>));
    run_with_provider(request, tx, factory).await;
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event.r#type.expect("events carry a type"));
    }
    events
}

fn actions(events: &[api::response_event::Type]) -> Vec<client_action::Action> {
    events
        .iter()
        .filter_map(|event| match event {
            api::response_event::Type::ClientActions(actions) => Some(actions.actions.clone()),
            _ => None,
        })
        .flatten()
        .filter_map(|action| action.action)
        .collect()
}

fn added_messages(events: &[api::response_event::Type]) -> Vec<api::Message> {
    actions(events)
        .into_iter()
        .filter_map(|action| match action {
            client_action::Action::AddMessagesToTask(add) => Some(add.messages),
            _ => None,
        })
        .flatten()
        .collect()
}

fn finish_reason(
    events: &[api::response_event::Type],
) -> api::response_event::stream_finished::Reason {
    match events.last() {
        Some(api::response_event::Type::Finished(finished)) => {
            finished.reason.clone().expect("Finished carries a reason")
        }
        other => panic!("the stream must end with Finished, got {other:?}"),
    }
}

/// The root task as the client sends it back: id plus the server data from the last turn.
fn root_task(events: &[api::response_event::Type]) -> api::Task {
    let mut task = api::Task::default();
    for action in actions(events) {
        match action {
            client_action::Action::CreateTask(create) => task.id = create.task.unwrap().id,
            client_action::Action::UpdateTaskServerData(update) => {
                task.server_data = update.server_data
            }
            _ => {}
        }
    }
    task
}

fn tool_request(id: &str, name: &str, arguments: serde_json::Value) -> Message {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("tool arguments are an object");
    };
    Message::assistant().with_tool_request(
        id,
        Ok(CallToolRequestParams::new(name.to_owned()).with_arguments(arguments)),
    )
}

#[tokio::test]
async fn first_turn_creates_the_root_task_echoes_the_query_and_streams_text() {
    let provider = ScriptedProvider::new(vec![vec![
        Message::assistant().with_text("Hello"),
        Message::assistant().with_text(" there"),
    ]]);
    let events = run_turn(request(user_query("hi"), Vec::new()), provider.clone()).await;

    assert!(
        matches!(events.first(), Some(api::response_event::Type::Init(init)) if !init.conversation_id.is_empty())
    );
    let actions = actions(&events);
    assert!(matches!(
        actions.first(),
        Some(client_action::Action::CreateTask(_))
    ));
    let added = added_messages(&events);
    assert!(matches!(&added[0].message, Some(Kind::UserQuery(query)) if query.query == "hi"));
    assert!(matches!(&added[1].message, Some(Kind::AgentOutput(output)) if output.text == "Hello"));
    let appended = actions
        .iter()
        .find_map(|action| match action {
            client_action::Action::AppendToMessageContent(append) => Some(append.clone()),
            _ => None,
        })
        .expect("the second chunk is appended");
    assert_eq!(appended.message.as_ref().unwrap().id, added[1].id);
    assert_eq!(appended.mask.unwrap().paths, vec![AGENT_OUTPUT_TEXT_PATH]);
    assert!(matches!(
        finish_reason(&events),
        api::response_event::stream_finished::Reason::Done(_)
    ));

    let calls = provider.calls.lock().unwrap();
    assert_eq!(calls[0].2, vec!["run_shell_command", "read_files"]);
    assert!(calls[0].0.contains("coding agent"));
}

#[tokio::test]
async fn tool_calls_go_to_the_client_and_their_results_resume_the_turn() {
    let provider = ScriptedProvider::new(vec![
        vec![tool_request(
            "toolu_1",
            "run_shell_command",
            json!({ "command": "ls", "read_only": true, "risky": false }),
        )],
        vec![Message::assistant().with_text("There are 2 files.")],
    ]);
    let first = run_turn(
        request(user_query("how many files?"), Vec::new()),
        provider.clone(),
    )
    .await;
    let call = added_messages(&first)
        .into_iter()
        .find_map(|message| match message.message {
            Some(Kind::ToolCall(call)) => Some(call),
            _ => None,
        })
        .expect("the tool call reaches the client");
    assert_eq!(call.tool_call_id, "toolu_1");
    let Some(api::message::tool_call::Tool::RunShellCommand(shell)) = call.tool else {
        panic!("expected a shell command");
    };
    assert_eq!(shell.command, "ls");
    assert!(shell.is_read_only && !shell.is_risky);
    assert!(matches!(
        finish_reason(&first),
        api::response_event::stream_finished::Reason::Done(_)
    ));

    let second = run_turn(
        request(shell_result("toolu_1", "a\nb", 0), vec![root_task(&first)]),
        provider.clone(),
    )
    .await;
    assert!(
        !actions(&second)
            .iter()
            .any(|action| matches!(action, client_action::Action::CreateTask(_)))
    );
    let added = added_messages(&second);
    assert!(
        matches!(&added[0].message, Some(Kind::ToolCallResult(result)) if result.tool_call_id == "toolu_1")
    );
    assert!(added.iter().any(|message| matches!(&message.message, Some(Kind::AgentOutput(output)) if output.text == "There are 2 files.")));

    let calls = provider.calls.lock().unwrap();
    let sent = serde_json::to_string(&calls[1].1).unwrap();
    assert!(
        sent.contains("Exit code 0"),
        "the model sees the shell output: {sent}"
    );
    assert!(
        sent.contains("how many files?"),
        "history survives via server_data"
    );
}

#[tokio::test]
async fn unanswered_tool_calls_are_cancelled_before_the_next_query() {
    let provider = ScriptedProvider::new(vec![
        vec![tool_request(
            "toolu_9",
            "run_shell_command",
            json!({ "command": "rm -rf build", "read_only": false, "risky": true }),
        )],
        vec![Message::assistant().with_text("OK, I won't.")],
    ]);
    let first = run_turn(
        request(user_query("clean up"), Vec::new()),
        provider.clone(),
    )
    .await;
    let second = run_turn(
        request(user_query("never mind"), vec![root_task(&first)]),
        provider.clone(),
    )
    .await;

    let added = added_messages(&second);
    assert!(added.iter().any(|message| matches!(
        &message.message,
        Some(Kind::ToolCallResult(result))
            if result.tool_call_id == "toolu_9"
                && matches!(result.result, Some(api::message::tool_call_result::Result::Cancel(())))
    )));
    let calls = provider.calls.lock().unwrap();
    let sent = serde_json::to_string(&calls[1].1).unwrap();
    assert!(sent.contains(CANCELLED_TOOL_RESULT));
}

fn with_mcp(mut request: api::Request) -> api::Request {
    request
        .settings
        .as_mut()
        .unwrap()
        .supported_tools
        .push(api::ToolType::CallMcpTool as i32);
    request.mcp_context = Some(api::request::McpContext {
        servers: vec![api::request::mcp_context::McpServer {
            id: "0b7e5c1e-0000-4000-8000-000000000001".to_owned(),
            name: "linear".to_owned(),
            tools: vec![api::request::mcp_context::McpTool {
                name: "search_issues".to_owned(),
                description: "Search issues".to_owned(),
                input_schema: None,
            }],
            ..Default::default()
        }],
        ..Default::default()
    });
    request
}

fn mcp_result(tool_call_id: &str, text: &str) -> api::request::input::Type {
    use api::call_mcp_tool_result::success::{Result as Part, result};
    use api::request::input::user_inputs::{UserInput, user_input};
    api::request::input::Type::UserInputs(api::request::input::UserInputs {
        inputs: vec![UserInput {
            input: Some(user_input::Input::ToolCallResult(
                api::request::input::ToolCallResult {
                    tool_call_id: tool_call_id.to_owned(),
                    result: Some(api::request::input::tool_call_result::Result::CallMcpTool(
                        api::CallMcpToolResult {
                            result: Some(api::call_mcp_tool_result::Result::Success(
                                api::call_mcp_tool_result::Success {
                                    results: vec![Part {
                                        result: Some(result::Result::Text(result::Text {
                                            text: text.to_owned(),
                                        })),
                                    }],
                                },
                            )),
                        },
                    )),
                },
            )),
        }],
    })
}

#[tokio::test]
async fn mcp_tools_are_offered_called_on_the_client_and_their_results_resume_the_turn() {
    let provider = ScriptedProvider::new(vec![
        vec![tool_request(
            "toolu_m",
            "mcp__linear__search_issues",
            json!({ "query": "login bug", "limit": 5 }),
        )],
        vec![Message::assistant().with_text("Found APP-1.")],
    ]);
    let first = run_turn(
        with_mcp(request(user_query("find the login bug"), Vec::new())),
        provider.clone(),
    )
    .await;
    let call = added_messages(&first)
        .into_iter()
        .find_map(|message| match message.message {
            Some(Kind::ToolCall(call)) => Some(call),
            _ => None,
        })
        .expect("the MCP call reaches the client");
    let Some(api::message::tool_call::Tool::CallMcpTool(mcp)) = call.tool else {
        panic!("expected an MCP tool call");
    };
    assert_eq!(mcp.name, "search_issues");
    assert_eq!(mcp.server_id, "0b7e5c1e-0000-4000-8000-000000000001");
    assert!(mcp.args.unwrap().fields.contains_key("query"));

    let second = run_turn(
        with_mcp(request(
            mcp_result("toolu_m", "APP-1: login fails"),
            vec![root_task(&first)],
        )),
        provider.clone(),
    )
    .await;
    assert!(added_messages(&second).iter().any(|message| matches!(&message.message, Some(Kind::AgentOutput(output)) if output.text == "Found APP-1.")));

    let calls = provider.calls.lock().unwrap();
    assert_eq!(
        calls[0].2,
        vec![
            "run_shell_command",
            "read_files",
            "mcp__linear__search_issues"
        ]
    );
    let sent = serde_json::to_string(&calls[1].1).unwrap();
    assert!(
        sent.contains("APP-1: login fails"),
        "the model sees the MCP result: {sent}"
    );
}

#[tokio::test]
async fn missing_key_or_unsupported_model_still_starts_and_finishes_the_stream() {
    let mut no_key = request(user_query("hi"), Vec::new());
    no_key.settings.as_mut().unwrap().api_keys = None;
    let events = run_turn(no_key, ScriptedProvider::new(Vec::new())).await;
    assert!(matches!(
        events.first(),
        Some(api::response_event::Type::Init(_))
    ));
    assert!(matches!(
        finish_reason(&events),
        api::response_event::stream_finished::Reason::InvalidApiKey(_)
    ));

    let mut other_model = request(user_query("hi"), Vec::new());
    other_model
        .settings
        .as_mut()
        .unwrap()
        .model_config
        .as_mut()
        .unwrap()
        .base = "gpt-5".to_owned();
    let events = run_turn(other_model, ScriptedProvider::new(Vec::new())).await;
    assert!(matches!(
        finish_reason(&events),
        api::response_event::stream_finished::Reason::InternalError(error) if error.message.contains("gpt-5")
    ));
}

#[test]
fn oauth_tokens_and_api_keys_build_providers() {
    assert!(anthropic_provider("sk-ant-oat01-token").is_ok());
    assert!(anthropic_provider("sk-ant-api03-key").is_ok());
}

/// Answers every call after a delay, standing in for a slow model so turns overlap.
struct SlowProvider;

#[async_trait]
impl Provider for SlowProvider {
    fn get_name(&self) -> &str {
        "slow"
    }

    async fn stream(
        &self,
        _model_config: &ModelConfig,
        _system: &str,
        _messages: &[Message],
        _tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        Ok(Box::pin(futures::stream::iter([Ok((
            Some(Message::assistant().with_text("done")),
            None,
        ))])))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn many_sessions_run_concurrently() {
    const SESSIONS: usize = 50;
    let started = tokio::time::Instant::now();
    let turns = (0..SESSIONS).map(|index| async move {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let factory: ProviderFactory =
            Arc::new(|_| Ok(Arc::new(SlowProvider) as Arc<dyn Provider>));
        run_with_provider(
            request(user_query(&format!("session {index}")), Vec::new()),
            tx,
            factory,
        )
        .await;
        let mut last = None;
        while let Ok(event) = rx.try_recv() {
            last = event.r#type;
        }
        last
    });
    let results = futures::future::join_all(turns).await;
    let elapsed = started.elapsed();

    assert!(results.iter().all(|last| matches!(
        last,
        Some(api::response_event::Type::Finished(finished))
            if matches!(finished.reason, Some(api::response_event::stream_finished::Reason::Done(_)))
    )));
    // Run one after another, 50 turns would take 10 seconds.
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "{SESSIONS} turns took {elapsed:?}"
    );
}

fn anthropic() -> ModelEndpoint {
    ModelEndpoint::Anthropic {
        credential: "sk-ant-api-test".to_owned(),
    }
}

fn cerebras() -> ModelEndpoint {
    ModelEndpoint::OpenAiChatCompletions {
        base_url: "https://api.cerebras.ai/v1".to_owned(),
        api_key: "csk-test".to_owned(),
    }
}

#[test]
fn provider_errors_map_to_reasons_the_user_can_act_on() {
    let forbidden = "Ran into this error: Authentication error: Authentication failed for https://api.anthropic.com/v1/messages. Status: 403 Forbidden. Response: OAuth token does not meet scope requirement any_of(user:inference) (provider request id: req_1).";
    assert!(matches!(
        classify_message_error(forbidden, "claude-opus-5-5", &anthropic()),
        TurnError::Internal(message)
            if message == "Anthropic refused the request: OAuth token does not meet scope requirement any_of(user:inference)"
    ));
    let unauthorized =
        "Authentication failed. Status: 401 Unauthorized. Response: invalid x-api-key";
    assert!(matches!(
        classify_message_error(unauthorized, "claude-opus-5-5", &anthropic()),
        TurnError::InvalidApiKey {
            provider: api::LlmProvider::Anthropic,
            ..
        }
    ));
    assert!(matches!(
        classify_message_error(
            "Status: 529 Overloaded. Response: overloaded_error",
            "m",
            &anthropic()
        ),
        TurnError::Unavailable
    ));
}

#[test]
fn custom_provider_errors_name_the_provider_host() {
    assert!(matches!(
        classify_message_error("Status: 400 Bad Request. Response: model not found", "llama-4", &cerebras()),
        TurnError::Internal(message) if message == "api.cerebras.ai refused the request: model not found"
    ));
    assert!(matches!(
        classify_message_error(
            "Status: 401 Unauthorized. Response: bad key",
            "llama-4",
            &cerebras()
        ),
        TurnError::InvalidApiKey {
            provider: api::LlmProvider::Unknown,
            ..
        }
    ));
}

/// Settings that select `selected` and register one Chat Completions provider serving
/// `qwen-3-coder` under the config key `cerebras:qwen-3-coder`.
fn settings_with_custom_provider(selected: &str, api_key: &str) -> api::request::Settings {
    use api::request::settings::custom_model_providers::{
        CustomEndpointSchema, CustomModel, CustomModelProvider,
    };
    api::request::Settings {
        model_config: Some(api::request::settings::ModelConfig {
            base: selected.to_owned(),
            ..Default::default()
        }),
        api_keys: Some(api::request::settings::ApiKeys {
            anthropic: "sk-ant-api-test".to_owned(),
            ..Default::default()
        }),
        custom_model_providers: Some(api::request::settings::CustomModelProviders {
            providers: vec![CustomModelProvider {
                base_url: "https://api.cerebras.ai/v1".to_owned(),
                api_key: api_key.to_owned(),
                models: vec![CustomModel {
                    slug: "qwen-3-coder".to_owned(),
                    config_key: "cerebras:qwen-3-coder".to_owned(),
                    ..Default::default()
                }],
                schema: CustomEndpointSchema::OpenaiChatCompletions as i32,
            }],
        }),
        ..Default::default()
    }
}

#[test]
fn a_custom_model_runs_on_its_provider_under_its_slug() {
    let settings = settings_with_custom_provider("cerebras:qwen-3-coder", "csk-test");
    let Ok((endpoint, model)) = resolve_model(Some(&settings)) else {
        panic!("the custom model should resolve");
    };
    assert_eq!(endpoint, cerebras());
    assert_eq!(model, "qwen-3-coder");
}

#[test]
fn claude_models_still_run_on_anthropic_alongside_custom_providers() {
    let settings = settings_with_custom_provider("claude-opus-5-5", "csk-test");
    let Ok((endpoint, model)) = resolve_model(Some(&settings)) else {
        panic!("the Claude model should resolve");
    };
    assert_eq!(endpoint, anthropic());
    assert_eq!(model, "claude-opus-5-5");
}

#[test]
fn a_custom_model_without_a_key_asks_for_one() {
    let settings = settings_with_custom_provider("cerebras:qwen-3-coder", " ");
    assert!(matches!(
        resolve_model(Some(&settings)),
        Err(TurnError::InvalidApiKey {
            provider: api::LlmProvider::Unknown,
            model,
        }) if model == "qwen-3-coder"
    ));
}

#[test]
fn an_unknown_model_is_refused() {
    let settings = settings_with_custom_provider("gpt-9", "csk-test");
    assert!(matches!(
        resolve_model(Some(&settings)),
        Err(TurnError::Internal(_))
    ));
}

#[test]
fn custom_schemas_the_server_cannot_call_are_refused() {
    use api::request::settings::custom_model_providers::CustomEndpointSchema;
    let mut settings = settings_with_custom_provider("cerebras:qwen-3-coder", "csk-test");
    settings.custom_model_providers.as_mut().unwrap().providers[0].schema =
        CustomEndpointSchema::AnthropicMessages as i32;
    assert!(matches!(
        resolve_model(Some(&settings)),
        Err(TurnError::Internal(message)) if message.contains("ANTHROPIC_MESSAGES")
    ));
}

#[test]
fn responses_providers_run_on_the_responses_api() {
    use api::request::settings::custom_model_providers::CustomEndpointSchema;
    let mut settings = settings_with_custom_provider("cerebras:qwen-3-coder", "token");
    let provider = &mut settings.custom_model_providers.as_mut().unwrap().providers[0];
    provider.schema = CustomEndpointSchema::OpenaiResponses as i32;
    provider.base_url = "https://chatgpt.com/backend-api/codex".to_owned();
    let Ok((endpoint, model)) = resolve_model(Some(&settings)) else {
        panic!("the Responses model should resolve");
    };
    assert_eq!(
        endpoint,
        ModelEndpoint::OpenAiResponses {
            base_url: "https://chatgpt.com/backend-api/codex".to_owned(),
            api_key: "token".to_owned(),
        }
    );
    assert_eq!(model, "qwen-3-coder");
    assert_eq!(endpoint.display_name(), "ChatGPT");
}

#[test]
fn cerebras_gets_the_history_without_earlier_reasoning() {
    assert!(rejects_replayed_reasoning("https://api.cerebras.ai/v1"));
    assert!(!rejects_replayed_reasoning("https://api.deepseek.com/v1"));

    let history = vec![
        Message::user().with_text("what is this folder?"),
        Message::assistant().with_thinking("I should list it.", "sig"),
        Message::assistant()
            .with_redacted_thinking("opaque")
            .with_text("Let me check."),
    ];
    let stripped = without_reasoning(&history);
    assert_eq!(stripped.len(), 2, "a reasoning-only message is dropped");
    assert_eq!(stripped[1].content.len(), 1);
    assert!(matches!(
        stripped[1].content[0],
        MessageContentBlock::Text(_)
    ));
}

fn voice_request(input: api::request::input::Type) -> api::Request {
    let mut request = request(input, Vec::new());
    request.metadata = Some(api::request::Metadata {
        logging: [(
            prompt::VOICE_CONVERSATION_METADATA_KEY.to_owned(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::BoolValue(true)),
            },
        )]
        .into(),
        ..Default::default()
    });
    request
}

fn agent_texts(events: &[api::response_event::Type]) -> Vec<String> {
    added_messages(events)
        .into_iter()
        .filter_map(|message| match message.message {
            Some(Kind::AgentOutput(output)) => Some(output.text),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn voice_replies_without_a_spoken_tag_get_a_narration() {
    let provider = ScriptedProvider::new(vec![
        vec![Message::assistant().with_text("There are 2 modified files.")],
        vec![Message::assistant().with_text("You have two uncommitted changes.")],
    ]);
    let events = run_turn(voice_request(user_query("status?")), provider.clone()).await;

    assert_eq!(
        agent_texts(&events),
        vec![
            "There are 2 modified files.",
            "<spoken>You have two uncommitted changes.</spoken>",
        ]
    );
    let calls = provider.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].0, prompt::NARRATION_SYSTEM_PROMPT);
    assert!(!root_task(&events).server_data.contains("uncommitted"));
}

#[tokio::test]
async fn spoken_replies_tool_calls_and_non_voice_turns_get_no_narration() {
    let spoken = ScriptedProvider::new(vec![vec![
        Message::assistant().with_text("<spoken>All good.</spoken> Done."),
    ]]);
    run_turn(voice_request(user_query("hi")), spoken.clone()).await;
    assert_eq!(spoken.calls.lock().unwrap().len(), 1);

    let tool = ScriptedProvider::new(vec![vec![tool_request(
        "toolu_1",
        "run_shell_command",
        json!({ "command": "ls", "read_only": true, "risky": false }),
    )]]);
    run_turn(voice_request(user_query("ls")), tool.clone()).await;
    assert_eq!(tool.calls.lock().unwrap().len(), 1);

    let plain = ScriptedProvider::new(vec![vec![Message::assistant().with_text("Done.")]]);
    run_turn(request(user_query("hi"), Vec::new()), plain.clone()).await;
    assert_eq!(plain.calls.lock().unwrap().len(), 1);
}
