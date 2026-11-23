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

/// The client's copy of the conversation, built from the server's actions the way the client
/// builds it, and sent back as `task_context`: the root plus subtasks whose subagent call is open.
#[derive(Default)]
struct Client {
    tasks: Vec<api::Task>,
}

impl Client {
    fn apply(&mut self, events: &[api::response_event::Type]) {
        for action in actions(events) {
            match action {
                client_action::Action::CreateTask(create) => {
                    let task = create.task.unwrap();
                    if let Some(parent) = task
                        .dependencies
                        .as_ref()
                        .map(|dependencies| dependencies.parent_task_id.clone())
                        .filter(|parent| !parent.is_empty())
                    {
                        // The client resolves a subtask through its parent's subagent call.
                        assert!(
                            self.task(&parent).messages.iter().any(|message| matches!(
                                &message.message,
                                Some(Kind::ToolCall(ToolCall {
                                    tool: Some(api::message::tool_call::Tool::Subagent(call)),
                                    ..
                                })) if call.task_id == task.id
                            )),
                            "the subagent call precedes its subtask"
                        );
                    }
                    self.tasks.push(task);
                }
                client_action::Action::AddMessagesToTask(add) => {
                    self.task_mut(&add.task_id).messages.extend(add.messages)
                }
                client_action::Action::UpdateTaskServerData(update) => {
                    self.task_mut(&update.task_id).server_data = update.server_data
                }
                _ => {}
            }
        }
    }

    fn task(&self, id: &str) -> &api::Task {
        self.tasks
            .iter()
            .find(|task| task.id == id)
            .unwrap_or_else(|| panic!("the client has no task {id}"))
    }

    fn task_mut(&mut self, id: &str) -> &mut api::Task {
        self.tasks
            .iter_mut()
            .find(|task| task.id == id)
            .unwrap_or_else(|| panic!("the client has no task {id}"))
    }

    fn root(&self) -> &api::Task {
        self.tasks.iter().find(|task| is_root(task)).unwrap()
    }

    /// The open CLI subtask, if any.
    fn cli_task(&self) -> Option<String> {
        self.tasks
            .iter()
            .find(|task| is_root(task))
            .and_then(CliSubtask::open_in)
            .map(|cli| cli.task_id)
    }

    fn task_context(&self) -> Vec<api::Task> {
        let open = self.cli_task();
        self.tasks
            .iter()
            .filter(|task| is_root(task) || Some(&task.id) == open.as_ref())
            .cloned()
            .collect()
    }

    async fn send(
        &mut self,
        input: api::request::input::Type,
        provider: &Arc<ScriptedProvider>,
    ) -> Vec<api::response_event::Type> {
        let events = run_turn(lrc_request(input, self.task_context()), provider.clone()).await;
        assert!(matches!(
            finish_reason(&events),
            api::response_event::stream_finished::Reason::Done(_)
        ));
        self.apply(&events);
        events
    }
}

/// A request from a client that supports long-running commands, as Warp's does.
fn lrc_request(input: api::request::input::Type, tasks: Vec<api::Task>) -> api::Request {
    let mut request = request(input, tasks);
    let settings = request.settings.as_mut().unwrap();
    settings.supports_long_running_commands = true;
    settings.supported_tools.extend([
        api::ToolType::WriteToLongRunningShellCommand as i32,
        api::ToolType::ReadShellCommandOutput as i32,
    ]);
    settings.supported_cli_agent_tools = vec![
        api::ToolType::WriteToLongRunningShellCommand as i32,
        api::ToolType::ReadShellCommandOutput as i32,
        api::ToolType::ReadFiles as i32,
    ];
    request
}

fn single_input(
    input: api::request::input::user_inputs::user_input::Input,
) -> api::request::input::Type {
    api::request::input::Type::UserInputs(api::request::input::UserInputs {
        inputs: vec![api::request::input::user_inputs::UserInput { input: Some(input) }],
    })
}

fn query_for(text: &str, agent: api::AgentType) -> api::request::input::Type {
    single_input(
        api::request::input::user_inputs::user_input::Input::UserQuery(
            api::request::input::UserQuery {
                query: text.to_owned(),
                intended_agent: agent as i32,
                ..Default::default()
            },
        ),
    )
}

const FIND: &str = "find /Users/me -name '*.log' | sort | head";

fn snapshot(output: &str) -> api::LongRunningShellCommandSnapshot {
    api::LongRunningShellCommandSnapshot {
        output: output.to_owned(),
        cursor: "<|cursor|>".to_owned(),
        command_id: "block-1".to_owned(),
        ..Default::default()
    }
}

fn tool_result_input(
    tool_call_id: &str,
    result: api::request::input::tool_call_result::Result,
) -> api::request::input::Type {
    single_input(
        api::request::input::user_inputs::user_input::Input::ToolCallResult(
            api::request::input::ToolCallResult {
                tool_call_id: tool_call_id.to_owned(),
                result: Some(result),
            },
        ),
    )
}

fn added_to(events: &[api::response_event::Type], task_id: &str) -> Vec<api::Message> {
    actions(events)
        .into_iter()
        .filter_map(|action| match action {
            client_action::Action::AddMessagesToTask(add) if add.task_id == task_id => {
                Some(add.messages)
            }
            _ => None,
        })
        .flatten()
        .collect()
}

fn texts(messages: &[api::Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match &message.message {
            Some(Kind::AgentOutput(output)) => Some(output.text.clone()),
            _ => None,
        })
        .collect()
}

fn tool_calls(messages: &[api::Message]) -> Vec<api::message::tool_call::Tool> {
    messages
        .iter()
        .filter_map(|message| match &message.message {
            Some(Kind::ToolCall(call)) => call.tool.clone(),
            _ => None,
        })
        .collect()
}

/// Replays the client's requests for a command that outlives the client's wait: the snapshot hands
/// the command to a CLI subagent, a follow-up typed while it runs reaches the agent with the
/// output, the agent stops it with Ctrl-C, and the finished command returns the turn to the root.
#[tokio::test]
async fn a_long_running_command_is_monitored_and_the_agent_answers_while_it_runs() {
    let provider = ScriptedProvider::new(vec![
        vec![tool_request(
            "toolu_1",
            "run_shell_command",
            json!({ "command": FIND, "read_only": true, "risky": false }),
        )],
        vec![Message::assistant().with_text("It's still searching. I'll keep watching.")],
        vec![
            Message::assistant().with_text("Not yet, stopping it now."),
            tool_request("toolu_2", "stop_shell_command", json!({})),
        ],
        vec![Message::assistant().with_text("I stopped it. It found 2 logs so far.")],
    ]);
    let mut client = Client::default();

    client
        .send(
            query_for("find my log files", api::AgentType::Primary),
            &provider,
        )
        .await;
    let root_id = client.root().id.clone();

    // The client stops waiting after a few seconds and reports a snapshot.
    let events = client
        .send(
            tool_result_input(
                "toolu_1",
                api::request::input::tool_call_result::Result::RunShellCommand(
                    api::RunShellCommandResult {
                        command: FIND.to_owned(),
                        result: Some(
                            api::run_shell_command_result::Result::LongRunningCommandSnapshot(
                                snapshot("/Users/me/a.log\n"),
                            ),
                        ),
                        ..Default::default()
                    },
                ),
            ),
            &provider,
        )
        .await;
    let cli_id = client
        .cli_task()
        .expect("the snapshot opens a CLI subagent");
    let root_added = added_to(&events, &root_id);
    assert!(
        matches!(&root_added[0].message, Some(Kind::ToolCallResult(result)) if result.tool_call_id == "toolu_1")
    );
    let [api::message::tool_call::Tool::Subagent(subagent)] = &tool_calls(&root_added)[..] else {
        panic!("the root task announces the subagent");
    };
    assert_eq!(
        subagent.metadata,
        Some(api::message::tool_call::subagent::Metadata::Cli(
            api::message::tool_call::subagent::CliSubagent {
                command_id: "block-1".to_owned()
            }
        ))
    );
    assert!(texts(&root_added).is_empty());
    assert_eq!(
        texts(&added_to(&events, &cli_id)),
        vec!["It's still searching. I'll keep watching."]
    );
    {
        let calls = provider.calls.lock().unwrap();
        let (system, sent, tools) = &calls[1];
        assert!(system.contains("Monitoring a running command"));
        assert_eq!(
            tools,
            &vec![
                "read_files",
                "read_shell_command_output",
                "write_to_shell_command",
                "stop_shell_command"
            ]
        );
        let sent = serde_json::to_string(sent).unwrap();
        assert!(sent.contains("still running (command_id: block-1)"));
    }

    // While the agent monitors the command, the client routes follow-ups to the CLI subtask.
    let events = client
        .send(
            query_for("Hello? Have you stopped it?", api::AgentType::Cli),
            &provider,
        )
        .await;
    assert!(added_to(&events, &root_id).is_empty());
    let cli_added = added_to(&events, &cli_id);
    assert!(
        matches!(&cli_added[0].message, Some(Kind::UserQuery(query)) if query.query == "Hello? Have you stopped it?")
    );
    assert_eq!(texts(&cli_added), vec!["Not yet, stopping it now."]);
    let [api::message::tool_call::Tool::WriteToLongRunningShellCommand(write)] =
        &tool_calls(&cli_added)[..]
    else {
        panic!("the agent sends Ctrl-C to the command");
    };
    assert_eq!(write.command_id, "block-1");
    assert_eq!(write.input, b"\x03");

    // The write's result shows the command exited, which closes the subagent.
    let events = client
        .send(
            tool_result_input(
                "toolu_2",
                api::request::input::tool_call_result::Result::WriteToLongRunningShellCommand(
                    api::WriteToLongRunningShellCommandResult {
                        result: Some(
                            api::write_to_long_running_shell_command_result::Result::CommandFinished(
                                api::ShellCommandFinished {
                                    output: "/Users/me/a.log\n/Users/me/b.log\n^C".to_owned(),
                                    exit_code: 130,
                                    command_id: "block-1".to_owned(),
                                    ..Default::default()
                                },
                            ),
                        ),
                    },
                ),
            ),
            &provider,
        )
        .await;
    assert!(
        matches!(&added_to(&events, &cli_id)[..], [message] if matches!(&message.message, Some(Kind::ToolCallResult(result)) if result.tool_call_id == "toolu_2"))
    );
    let root_added = added_to(&events, &root_id);
    assert!(matches!(
        &root_added[0].message,
        Some(Kind::ToolCallResult(ToolCallResult {
            result: Some(api::message::tool_call_result::Result::Subagent(_)),
            ..
        }))
    ));
    assert_eq!(
        texts(&root_added),
        vec!["I stopped it. It found 2 logs so far."]
    );
    assert_eq!(client.cli_task(), None);

    let calls = provider.calls.lock().unwrap();
    let (system, sent, tools) = &calls[3];
    assert!(!system.contains("Monitoring a running command"));
    assert!(tools.contains(&"run_shell_command".to_owned()));
    let sent = serde_json::to_string(sent).unwrap();
    for expected in [
        "find my log files",
        "Have you stopped it?",
        "exit code 130",
        "b.log",
    ] {
        assert!(sent.contains(expected), "the model sees {expected}: {sent}");
    }
}

/// The screenshot's case: the user writes while the agent's command runs and no subagent is open
/// yet, so the client sends a `CliAgentUserQuery` and an optimistic subtask that waits for the
/// server's `CreateTask`.
#[tokio::test]
async fn a_follow_up_about_a_running_command_opens_a_subagent_and_gets_an_answer() {
    let provider = ScriptedProvider::new(vec![
        vec![tool_request(
            "toolu_1",
            "run_shell_command",
            json!({ "command": FIND, "read_only": true, "risky": false }),
        )],
        vec![Message::assistant().with_text("No, it's still running. It has found a.log.")],
    ]);
    let mut client = Client::default();
    client
        .send(
            query_for("find my log files", api::AgentType::Primary),
            &provider,
        )
        .await;
    let root_id = client.root().id.clone();

    use api::request::input::user_inputs::user_input::Input;
    let events = client
        .send(
            single_input(Input::CliAgentUserQuery(
                api::request::input::CliAgentUserQuery {
                    user_query: Some(api::request::input::UserQuery {
                        query: "Hello? Have you stopped it?".to_owned(),
                        intended_agent: api::AgentType::Cli as i32,
                        ..Default::default()
                    }),
                    running_command: Some(api::RunningShellCommand {
                        command: FIND.to_owned(),
                        snapshot: Some(snapshot("/Users/me/a.log\n")),
                    }),
                    run_shell_command_tool_call_id: "toolu_1".to_owned(),
                },
            )),
            &provider,
        )
        .await;

    let cli_id = client
        .cli_task()
        .expect("the follow-up opens a CLI subagent");
    let root_added = added_to(&events, &root_id);
    assert!(matches!(
        &root_added[0].message,
        Some(Kind::ToolCallResult(ToolCallResult {
            tool_call_id,
            result: Some(api::message::tool_call_result::Result::RunShellCommand(_)),
            ..
        })) if tool_call_id == "toolu_1"
    ));
    assert!(matches!(
        &tool_calls(&root_added)[..],
        [api::message::tool_call::Tool::Subagent(_)]
    ));
    let cli_added = added_to(&events, &cli_id);
    assert!(
        matches!(&cli_added[0].message, Some(Kind::UserQuery(query)) if query.query == "Hello? Have you stopped it?")
    );
    assert_eq!(
        texts(&cli_added),
        vec!["No, it's still running. It has found a.log."]
    );

    let calls = provider.calls.lock().unwrap();
    let sent = serde_json::to_string(&calls[1].1).unwrap();
    assert!(sent.contains("Have you stopped it?"));
    assert!(sent.contains("is still running in the terminal (command_id: block-1)"));
    assert!(sent.contains("/Users/me/a.log"));
}

async fn client_monitoring_a_command(provider: &Arc<ScriptedProvider>) -> Client {
    let mut client = Client::default();
    client
        .send(
            query_for("find my log files", api::AgentType::Primary),
            provider,
        )
        .await;
    client
        .send(
            tool_result_input(
                "toolu_1",
                api::request::input::tool_call_result::Result::RunShellCommand(
                    api::RunShellCommandResult {
                        command: FIND.to_owned(),
                        result: Some(
                            api::run_shell_command_result::Result::LongRunningCommandSnapshot(
                                snapshot(""),
                            ),
                        ),
                        ..Default::default()
                    },
                ),
            ),
            provider,
        )
        .await;
    assert!(client.cli_task().is_some());
    client
}

#[tokio::test]
async fn a_poll_interrupted_by_a_follow_up_is_answered_as_cancelled() {
    let provider = ScriptedProvider::new(vec![
        vec![tool_request(
            "toolu_1",
            "run_shell_command",
            json!({ "command": FIND, "read_only": true, "risky": false }),
        )],
        vec![tool_request(
            "toolu_2",
            "read_shell_command_output",
            json!({ "until_complete": true }),
        )],
        vec![Message::assistant().with_text("It's still going.")],
    ]);
    let mut client = client_monitoring_a_command(&provider).await;
    let cli_id = client.cli_task().unwrap();
    let poll = tool_calls(&client.task(&cli_id).messages);
    assert!(matches!(
        &poll[..],
        [api::message::tool_call::Tool::ReadShellCommandOutput(read)] if read.command_id == "block-1"
    ));

    // The client drops the pending poll when the user sends a follow-up.
    let events = client
        .send(query_for("status?", api::AgentType::Cli), &provider)
        .await;
    let cli_added = added_to(&events, &cli_id);
    assert!(matches!(
        &cli_added[0].message,
        Some(Kind::ToolCallResult(ToolCallResult {
            tool_call_id,
            result: Some(api::message::tool_call_result::Result::Cancel(())),
            ..
        })) if tool_call_id == "toolu_2"
    ));
    assert_eq!(texts(&cli_added), vec!["It's still going."]);
}

#[tokio::test]
async fn a_query_for_the_main_agent_closes_the_subagent() {
    let provider = ScriptedProvider::new(vec![
        vec![tool_request(
            "toolu_1",
            "run_shell_command",
            json!({ "command": FIND, "read_only": true, "risky": false }),
        )],
        vec![Message::assistant().with_text("Watching it.")],
        vec![Message::assistant().with_text("Sure.")],
    ]);
    let mut client = client_monitoring_a_command(&provider).await;
    let root_id = client.root().id.clone();

    let events = client
        .send(
            query_for("something else", api::AgentType::Primary),
            &provider,
        )
        .await;
    assert_eq!(client.cli_task(), None);
    let root_added = added_to(&events, &root_id);
    assert!(matches!(
        &root_added[0].message,
        Some(Kind::ToolCallResult(ToolCallResult {
            result: Some(api::message::tool_call_result::Result::Subagent(_)),
            ..
        }))
    ));
    assert_eq!(texts(&root_added), vec!["Sure."]);
}

/// After the user takes over and the command exits, the client resumes the conversation with the
/// finished block as context.
#[tokio::test]
#[allow(deprecated)]
async fn resuming_after_a_take_over_reports_the_finished_command() {
    let provider = ScriptedProvider::new(vec![
        vec![tool_request(
            "toolu_1",
            "run_shell_command",
            json!({ "command": FIND, "read_only": true, "risky": false }),
        )],
        vec![Message::assistant().with_text("Watching it.")],
        vec![Message::assistant().with_text("The search is done.")],
    ]);
    let mut client = client_monitoring_a_command(&provider).await;
    let root_id = client.root().id.clone();

    let mut request = lrc_request(
        api::request::input::Type::ResumeConversation(Default::default()),
        client.task_context(),
    );
    request.input.as_mut().unwrap().context = Some(api::InputContext {
        executed_shell_commands: vec![api::ExecutedShellCommand {
            command: FIND.to_owned(),
            output: "/Users/me/a.log".to_owned(),
            exit_code: 0,
            command_id: "block-1".to_owned(),
            finished_ts: Some(prost_types::Timestamp::default()),
            ..Default::default()
        }],
        ..Default::default()
    });
    let events = run_turn(request, provider.clone()).await;
    client.apply(&events);

    assert_eq!(client.cli_task(), None);
    assert_eq!(
        texts(&added_to(&events, &root_id)),
        vec!["The search is done."]
    );
    let calls = provider.calls.lock().unwrap();
    let sent = serde_json::to_string(&calls[2].1).unwrap();
    assert!(sent.contains("finished with exit code 0"));
}

/// Asserts the model API accepts the conversation: every tool call has a result.
fn assert_every_tool_call_answered(messages: &[Message]) {
    let content = || messages.iter().flat_map(|message| &message.content);
    let answered: HashSet<&str> = content()
        .filter_map(|block| match block {
            MessageContentBlock::ToolResponse(response) => Some(response.id.as_str()),
            _ => None,
        })
        .collect();
    for block in content() {
        if let MessageContentBlock::ToolRequest(request) = block {
            assert!(
                answered.contains(request.id.as_str()),
                "tool call {} has no result",
                request.id
            );
        }
    }
}

#[allow(deprecated)]
fn resume_with_block(client: &Client, output: &str, finished: bool) -> api::Request {
    let mut request = lrc_request(
        api::request::input::Type::ResumeConversation(Default::default()),
        client.task_context(),
    );
    request.input.as_mut().unwrap().context = Some(api::InputContext {
        executed_shell_commands: vec![api::ExecutedShellCommand {
            command: FIND.to_owned(),
            output: output.to_owned(),
            command_id: "block-1".to_owned(),
            finished_ts: finished.then(prost_types::Timestamp::default),
            ..Default::default()
        }],
        ..Default::default()
    });
    request
}

/// Replays the client around a take over. The client cancels the agent's pending poll locally and
/// sends nothing when the user takes over; handing control back resumes the subtask with the
/// running block, and a command that exits while the user is in control resumes the root task with
/// the finished block. The subagent must stay the block's subagent through the hand back, the
/// cancelled poll must not leave the model conversation unanswered, and the finished command must
/// close the subagent.
#[tokio::test]
async fn take_over_and_hand_back_keep_the_monitored_conversation_going() {
    let provider = ScriptedProvider::new(vec![
        vec![tool_request(
            "toolu_1",
            "run_shell_command",
            json!({ "command": FIND, "read_only": true, "risky": false }),
        )],
        vec![tool_request(
            "toolu_2",
            "read_shell_command_output",
            json!({ "wait_seconds": 30 }),
        )],
        vec![
            Message::assistant().with_text("Back on it, it has found a.log."),
            tool_request(
                "toolu_3",
                "read_shell_command_output",
                json!({ "wait_seconds": 30 }),
            ),
        ],
        vec![Message::assistant().with_text("The search is done: a.log and b.log.")],
    ]);
    let mut client = client_monitoring_a_command(&provider).await;
    let root_id = client.root().id.clone();
    let cli_id = client.cli_task().unwrap();

    // Hand back while the command runs: the client resumes the subtask with the running block.
    let events = run_turn(
        resume_with_block(&client, "/Users/me/a.log\n", false),
        provider.clone(),
    )
    .await;
    assert!(matches!(
        finish_reason(&events),
        api::response_event::stream_finished::Reason::Done(_)
    ));
    client.apply(&events);
    assert_eq!(client.cli_task(), Some(cli_id.clone()));
    assert!(
        !actions(&events)
            .iter()
            .any(|action| matches!(action, client_action::Action::CreateTask(_))),
        "the hand back keeps the block's subagent"
    );
    assert!(added_to(&events, &root_id).is_empty());
    let cli_added = added_to(&events, &cli_id);
    assert!(matches!(
        &cli_added[0].message,
        Some(Kind::ToolCallResult(ToolCallResult {
            tool_call_id,
            result: Some(api::message::tool_call_result::Result::Cancel(())),
            ..
        })) if tool_call_id == "toolu_2"
    ));
    assert_eq!(texts(&cli_added), vec!["Back on it, it has found a.log."]);
    assert!(matches!(
        &tool_calls(&cli_added)[..],
        [api::message::tool_call::Tool::ReadShellCommandOutput(read)] if read.command_id == "block-1"
    ));
    {
        let calls = provider.calls.lock().unwrap();
        let (system, sent, _) = &calls[2];
        assert!(system.contains("Monitoring a running command"));
        assert_every_tool_call_answered(sent);
        let sent = serde_json::to_string(sent).unwrap();
        assert!(sent.contains("handed control of the running command"));
    }

    // The user takes over again and the command exits: the client resumes the root task.
    let events = run_turn(
        resume_with_block(&client, "/Users/me/a.log\n/Users/me/b.log\n", true),
        provider.clone(),
    )
    .await;
    client.apply(&events);
    assert_eq!(client.cli_task(), None);
    let root_added = added_to(&events, &root_id);
    assert!(matches!(
        &root_added[..],
        [closing, ..] if matches!(&closing.message, Some(Kind::ToolCallResult(ToolCallResult {
            result: Some(api::message::tool_call_result::Result::Subagent(_)),
            ..
        })))
    ));
    assert_eq!(
        texts(&root_added),
        vec!["The search is done: a.log and b.log."]
    );
    assert!(matches!(
        &added_to(&events, &cli_id)[..],
        [message] if matches!(&message.message, Some(Kind::ToolCallResult(ToolCallResult {
            tool_call_id,
            result: Some(api::message::tool_call_result::Result::Cancel(())),
            ..
        })) if tool_call_id == "toolu_3")
    ));
    let calls = provider.calls.lock().unwrap();
    let (system, sent, tools) = &calls[3];
    assert!(!system.contains("Monitoring a running command"));
    assert!(tools.contains(&"run_shell_command".to_owned()));
    assert_every_tool_call_answered(sent);
    assert!(
        serde_json::to_string(sent)
            .unwrap()
            .contains("finished with exit code 0")
    );
}

/// With "send immediately", a follow-up typed while the agent polls the command is a plain query
/// for the CLI agent that cancels the poll; the agent is told to read the output it doesn't carry.
#[tokio::test]
async fn a_follow_up_sent_immediately_reaches_the_monitoring_agent() {
    let provider = ScriptedProvider::new(vec![
        vec![tool_request(
            "toolu_1",
            "run_shell_command",
            json!({ "command": FIND, "read_only": true, "risky": false }),
        )],
        vec![tool_request(
            "toolu_2",
            "read_shell_command_output",
            json!({ "wait_seconds": 30 }),
        )],
        vec![tool_request(
            "toolu_3",
            "read_shell_command_output",
            json!({}),
        )],
        vec![Message::assistant().with_text("It has found a.log so far.")],
    ]);
    let mut client = client_monitoring_a_command(&provider).await;
    let cli_id = client.cli_task().unwrap();

    let events = client
        .send(
            query_for("what has it found?", api::AgentType::Cli),
            &provider,
        )
        .await;
    let cli_added = added_to(&events, &cli_id);
    assert!(matches!(
        &cli_added[0].message,
        Some(Kind::ToolCallResult(ToolCallResult {
            tool_call_id,
            result: Some(api::message::tool_call_result::Result::Cancel(())),
            ..
        })) if tool_call_id == "toolu_2"
    ));
    assert!(
        matches!(&cli_added[1].message, Some(Kind::UserQuery(query)) if query.query == "what has it found?")
    );
    {
        let calls = provider.calls.lock().unwrap();
        let (_, sent, _) = &calls[2];
        assert_every_tool_call_answered(sent);
        let sent = serde_json::to_string(sent).unwrap();
        assert!(sent.contains("what has it found?"));
        assert!(sent.contains("call read_shell_command_output without a wait"));
    }

    let events = client
        .send(
            tool_result_input(
                "toolu_3",
                api::request::input::tool_call_result::Result::ReadShellCommandOutput(
                    api::ReadShellCommandOutputResult {
                        command: FIND.to_owned(),
                        result: Some(
                            api::read_shell_command_output_result::Result::LongRunningCommandSnapshot(
                                snapshot("/Users/me/a.log\n"),
                            ),
                        ),
                    },
                ),
            ),
            &provider,
        )
        .await;
    assert_eq!(client.cli_task(), Some(cli_id.clone()));
    assert_eq!(
        texts(&added_to(&events, &cli_id)),
        vec!["It has found a.log so far."]
    );
}
