//! One agent turn: rebuild the conversation from the request, add the new input, run the goose
//! agent loop (`oi-kit-agent`) until it answers or asks for client tools, and stream the result
//! to the client as Warp `ResponseEvent`s.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use oi_kit_agent::events::AgentEvent;
use oi_kit_agent::inference::{InferenceEffect, InferenceRunner};
use oi_kit_agent::machine::{EffectHandler, MachineSession, SessionLoader, StateMachine, Step};
use oi_kit_agent::operation::{Emitter, MachineEffect, Operation, OperationResult, not_applicable};
use oi_kit_provider_types::base::{MessageStream, Provider};
use oi_kit_provider_types::conversation::Conversation;
use oi_kit_provider_types::conversation::message::{Message, MessageContentBlock};
use oi_kit_provider_types::conversation::token_usage::ProviderUsage;
use oi_kit_provider_types::errors::ProviderError;
use oi_kit_provider_types::model::ModelConfig;
use oi_kit_providers::anthropic::AnthropicProviderBuilder;
use oi_kit_providers::api_client::{ApiClient, AuthMethod};
use oi_kit_providers::openai_compatible::OpenAiCompatibleProvider;
use prost_types::FieldMask;
use rmcp::model::{CallToolResult, ContentBlock, Role, Tool};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use warp_multi_agent_api as api;
use warp_multi_agent_api::client_action::{
    self, AddMessagesToTask, AppendToMessageContent, CreateTask, UpdateTaskServerData,
};
use warp_multi_agent_api::message::{
    AgentOutput, Message as Kind, ToolCall, ToolCallResult, UserQuery,
};
use warp_multi_agent_api::request::settings::custom_model_providers::CustomEndpointSchema;

use super::mcp::McpTools;
use super::monitor::{self, CliSubtask};
use super::responses::{self, ResponsesProvider};
use super::{helpers, prompt, tools};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com";
const OAUTH_TOKEN_PREFIX: &str = "sk-ant-oat";
const AGENT_OUTPUT_TEXT_PATH: &str = "agent_output.text";
const CANCELLED_TOOL_RESULT: &str = "The user cancelled this tool call before it ran.";

/// Why a turn ended without an answer, as the client's `StreamFinished` reasons.
pub(super) enum TurnError {
    InvalidApiKey {
        provider: api::LlmProvider,
        model: String,
    },
    Unavailable,
    Internal(String),
}

/// Where the selected model runs, with the user's credential for it.
#[derive(Debug, PartialEq, Eq)]
pub enum ModelEndpoint {
    Anthropic {
        credential: String,
    },
    /// A user-configured endpoint that speaks OpenAI Chat Completions.
    OpenAiChatCompletions {
        base_url: String,
        api_key: String,
    },
    /// An endpoint that speaks the OpenAI Responses API, such as ChatGPT's Codex backend.
    OpenAiResponses {
        base_url: String,
        api_key: String,
    },
}

impl ModelEndpoint {
    /// The provider's name in errors shown to the user.
    fn display_name(&self) -> String {
        match self {
            Self::Anthropic { .. } => "Anthropic".to_owned(),
            Self::OpenAiResponses { base_url, .. } if responses::is_codex_backend(base_url) => {
                "ChatGPT".to_owned()
            }
            Self::OpenAiChatCompletions { base_url, .. }
            | Self::OpenAiResponses { base_url, .. } => url::Url::parse(base_url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_else(|| base_url.clone()),
        }
    }

    pub(super) fn llm_provider(&self) -> api::LlmProvider {
        match self {
            Self::Anthropic { .. } => api::LlmProvider::Anthropic,
            Self::OpenAiChatCompletions { .. } | Self::OpenAiResponses { .. } => {
                api::LlmProvider::Unknown
            }
        }
    }
}

/// Builds the model provider for an endpoint.
pub type ProviderFactory = Arc<dyn Fn(&ModelEndpoint) -> Result<Arc<dyn Provider>> + Send + Sync>;

/// Runs one turn and sends its events to `out`. Always ends with `StreamFinished`.
pub(crate) async fn run_with_provider(
    request: api::Request,
    out: mpsc::UnboundedSender<api::ResponseEvent>,
    make_provider: ProviderFactory,
) {
    let mut stream = Stream::new(&request, out);
    let reason = match stream.run_turn(request, &make_provider).await {
        Ok(()) => api::response_event::stream_finished::Reason::Done(Default::default()),
        Err(TurnError::InvalidApiKey { provider, model }) => {
            log::warn!("Agent turn refused: no usable credential for {model}");
            api::response_event::stream_finished::Reason::InvalidApiKey(
                api::response_event::stream_finished::InvalidApiKey {
                    provider: provider as i32,
                    model_name: model,
                },
            )
        }
        Err(TurnError::Unavailable) => {
            api::response_event::stream_finished::Reason::LlmUnavailable(Default::default())
        }
        Err(TurnError::Internal(message)) => {
            log::warn!("Agent turn failed: {message}");
            api::response_event::stream_finished::Reason::InternalError(
                api::response_event::stream_finished::InternalError { message },
            )
        }
    };
    stream.send(api::response_event::Type::Finished(
        api::response_event::StreamFinished {
            reason: Some(reason),
            ..Default::default()
        },
    ));
}

/// The response stream of one request: event plumbing and message bookkeeping.
struct Stream {
    out: mpsc::UnboundedSender<api::ResponseEvent>,
    conversation_id: String,
    request_id: String,
    root_task_id: String,
    /// The task this turn's replies go to: the root task, or the open CLI subagent's subtask.
    task_id: String,
    cli: Option<CliSubtask>,
    /// The streamed text message the next text delta extends.
    open_text_message: Option<String>,
    mcp_tools: McpTools,
}

impl Stream {
    fn new(request: &api::Request, out: mpsc::UnboundedSender<api::ResponseEvent>) -> Self {
        let conversation_id = request
            .metadata
            .as_ref()
            .map(|metadata| metadata.conversation_id.clone())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let stream = Self {
            out,
            conversation_id,
            request_id: Uuid::new_v4().to_string(),
            root_task_id: String::new(),
            task_id: String::new(),
            cli: None,
            open_text_message: None,
            mcp_tools: McpTools::from_request(request),
        };
        // Init must precede every action, including on failures.
        stream.send(api::response_event::Type::Init(
            api::response_event::StreamInit {
                conversation_id: stream.conversation_id.clone(),
                request_id: stream.request_id.clone(),
                ..Default::default()
            },
        ));
        stream
    }

    fn send(&self, event: api::response_event::Type) {
        let _ = self.out.send(api::ResponseEvent {
            r#type: Some(event),
        });
    }

    fn actions(&self, actions: Vec<client_action::Action>) {
        if actions.is_empty() {
            return;
        }
        self.send(api::response_event::Type::ClientActions(
            api::response_event::ClientActions {
                actions: actions
                    .into_iter()
                    .map(|action| api::ClientAction {
                        action: Some(action),
                    })
                    .collect(),
            },
        ));
    }

    fn new_message(&self, kind: Kind) -> api::Message {
        self.new_message_in(&self.task_id, kind)
    }

    fn new_message_in(&self, task_id: &str, kind: Kind) -> api::Message {
        api::Message {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.to_owned(),
            request_id: self.request_id.clone(),
            timestamp: Some(prost_types::Timestamp::from(SystemTime::now())),
            message: Some(kind),
            ..Default::default()
        }
    }

    fn add_messages(&self, messages: Vec<api::Message>) -> client_action::Action {
        client_action::Action::AddMessagesToTask(AddMessagesToTask {
            task_id: self.task_id.clone(),
            messages,
        })
    }

    /// Adds messages to the tasks they name, keeping their order.
    fn add_messages_in_order(&self, messages: Vec<api::Message>) {
        let mut actions: Vec<client_action::Action> = Vec::new();
        for message in messages {
            if let Some(client_action::Action::AddMessagesToTask(add)) = actions.last_mut()
                && add.task_id == message.task_id
            {
                add.messages.push(message);
                continue;
            }
            actions.push(client_action::Action::AddMessagesToTask(
                AddMessagesToTask {
                    task_id: message.task_id.clone(),
                    messages: vec![message],
                },
            ));
        }
        self.actions(actions);
    }

    /// Opens a CLI subagent for a command that is still running, closing any other one first.
    fn open_cli_subagent(&mut self, command_id: String, command: &str) {
        if self
            .cli
            .as_ref()
            .is_some_and(|cli| cli.command_id == command_id)
        {
            return;
        }
        self.close_cli_subagent("The agent moved on to another command.");
        let cli = CliSubtask::new(command_id);
        let opening = self.new_message_in(&self.root_task_id, cli.opening_call(command));
        // The opening call must reach the root task before the subtask: the client finds the
        // subtask's command through it.
        self.actions(vec![
            client_action::Action::AddMessagesToTask(AddMessagesToTask {
                task_id: self.root_task_id.clone(),
                messages: vec![opening],
            }),
            client_action::Action::CreateTask(CreateTask {
                task: Some(cli.task(&self.root_task_id)),
            }),
        ]);
        self.cli = Some(cli);
    }

    fn close_cli_subagent(&mut self, summary: &str) {
        let Some(cli) = self.cli.take() else {
            return;
        };
        let closing = self.new_message_in(&self.root_task_id, cli.closing_result(summary));
        self.actions(vec![client_action::Action::AddMessagesToTask(
            AddMessagesToTask {
                task_id: self.root_task_id.clone(),
                messages: vec![closing],
            },
        )]);
    }

    fn stream_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let action = match &self.open_text_message {
            Some(message_id) => {
                client_action::Action::AppendToMessageContent(AppendToMessageContent {
                    task_id: self.task_id.clone(),
                    message: Some(api::Message {
                        id: message_id.clone(),
                        task_id: self.task_id.clone(),
                        request_id: self.request_id.clone(),
                        message: Some(Kind::AgentOutput(AgentOutput {
                            text: text.to_owned(),
                        })),
                        ..Default::default()
                    }),
                    mask: Some(FieldMask {
                        paths: vec![AGENT_OUTPUT_TEXT_PATH.to_owned()],
                    }),
                })
            }
            None => {
                let message = self.new_message(Kind::AgentOutput(AgentOutput {
                    text: text.to_owned(),
                }));
                self.open_text_message = Some(message.id.clone());
                self.add_messages(vec![message])
            }
        };
        self.actions(vec![action]);
    }

    fn tool_call(&mut self, tool_call_id: String, tool: api::message::tool_call::Tool) {
        self.open_text_message = None;
        let message = self.new_message(Kind::ToolCall(ToolCall {
            tool_call_id,
            tool: Some(tool),
        }));
        let action = self.add_messages(vec![message]);
        self.actions(vec![action]);
    }

    async fn run_turn(
        &mut self,
        request: api::Request,
        make_provider: &ProviderFactory,
    ) -> Result<(), TurnError> {
        let tasks = request
            .task_context
            .as_ref()
            .map(|context| context.tasks.as_slice())
            .unwrap_or(&[]);
        let root = tasks.iter().find(|task| is_root(task));
        let mut messages = match root {
            Some(task) => {
                self.root_task_id = task.id.clone();
                self.cli = CliSubtask::open_in(task);
                restore_conversation(task)
            }
            None => {
                self.root_task_id = Uuid::new_v4().to_string();
                self.actions(vec![client_action::Action::CreateTask(CreateTask {
                    task: Some(api::Task {
                        id: self.root_task_id.clone(),
                        ..Default::default()
                    }),
                })]);
                Vec::new()
            }
        };
        self.task_id = self.current_task_id();

        let input = request
            .input
            .as_ref()
            .ok_or_else(|| internal("the request has no input"))?;
        self.apply_input(input, tasks, &mut messages)?;

        let settings = request.settings.as_ref();
        let (endpoint, model) = resolve_model(settings)?;
        let mut tools = match (&self.cli, settings) {
            (Some(_), Some(settings)) if !settings.supported_cli_agent_tools.is_empty() => {
                tools::definitions(&settings.supported_cli_agent_tools)
            }
            (Some(_), _) => tools::definitions(
                &settings
                    .map(|settings| settings.supported_tools.clone())
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|tool| *tool != api::ToolType::RunShellCommand as i32)
                    .collect::<Vec<_>>(),
            ),
            (None, _) => tools::definitions(
                settings
                    .map(|settings| settings.supported_tools.as_slice())
                    .unwrap_or(&[]),
            ),
        };
        if self.cli.is_none() {
            tools.extend(self.mcp_tools.definitions());
        }
        let spoken_summary = prompt::wants_spoken_summary(request.metadata.as_ref());
        let mut system_prompt = prompt::system_prompt(
            input.context.as_ref(),
            spoken_summary,
            prompt::is_dictated(request.metadata.as_ref()),
            prompt::wants_voice_terms(request.metadata.as_ref()),
        );
        if let Some(cli) = &self.cli {
            system_prompt.push_str(&monitor::instructions(&cli.command_id));
        }

        let provider = make_provider(&endpoint).map_err(|error| internal(format!("{error:#}")))?;
        let messages = self
            .run_agent(messages, &model, &endpoint, provider, tools, system_prompt)
            .await?;
        if spoken_summary && let Some(settings) = settings {
            self.narrate_unspoken_reply(&messages, settings, make_provider)
                .await;
        }
        let server_data = serde_json::to_string(&messages)
            .map_err(|error| internal(format!("couldn't save the conversation: {error}")))?;
        self.actions(vec![client_action::Action::UpdateTaskServerData(
            UpdateTaskServerData {
                task_id: self.root_task_id.clone(),
                server_data,
            },
        )]);
        Ok(())
    }

    /// Streams a short `<spoken>` narration of the turn's final reply when the model didn't open
    /// it with one, so voice conversation mode still has something to read aloud. The narration
    /// goes to the client only and stays out of the saved model conversation.
    async fn narrate_unspoken_reply(
        &mut self,
        messages: &[Message],
        settings: &api::request::Settings,
        make_provider: &ProviderFactory,
    ) {
        let Some(reply) = unspoken_final_reply(messages) else {
            return;
        };
        match helpers::complete(
            settings,
            prompt::NARRATION_SYSTEM_PROMPT,
            reply,
            make_provider,
        )
        .await
        {
            Ok(narration) if !narration.trim().is_empty() => {
                self.open_text_message = None;
                self.stream_text(&format!("<spoken>{}</spoken>", narration.trim()));
            }
            Ok(_) => {}
            Err(_) => log::warn!("The spoken narration call failed"),
        }
    }

    fn current_task_id(&self) -> String {
        self.cli
            .as_ref()
            .map_or_else(|| self.root_task_id.clone(), |cli| cli.task_id.clone())
    }

    /// Adds the request's input to the model conversation and echoes it into the task history,
    /// which the client doesn't record itself. Opens a CLI subagent when the input shows a command
    /// still running, and closes the open one once its command finishes or the user addresses the
    /// main agent.
    fn apply_input(
        &mut self,
        input: &api::request::Input,
        tasks: &[api::Task],
        messages: &mut Vec<Message>,
    ) -> Result<(), TurnError> {
        use api::request::input::Type;
        use api::request::input::user_inputs::user_input::Input as UserInput;
        let mut tool_responses = Message::user();
        let mut answered = HashSet::new();
        let mut texts = Vec::new();
        let mut queries = Vec::new();
        let mut result_echoes = Vec::new();
        let mut addresses_main_agent = false;
        let mut asked_about_running = false;
        let mut running: Option<(String, String)> = None;
        let mut finished = HashSet::new();
        #[allow(deprecated)]
        let executed = input
            .context
            .as_ref()
            .map(|context| context.executed_shell_commands.as_slice())
            .unwrap_or(&[]);
        for command in executed {
            if command.finished_ts.is_some() {
                finished.insert(command.command_id.clone());
            }
        }
        match &input.r#type {
            Some(Type::UserInputs(user_inputs)) => {
                for user_input in &user_inputs.inputs {
                    match &user_input.input {
                        Some(UserInput::UserQuery(query)) => {
                            addresses_main_agent |=
                                query.intended_agent == api::AgentType::Primary as i32;
                            texts.push(query.query.clone());
                            queries.push(query.query.clone());
                        }
                        Some(UserInput::CliAgentUserQuery(cli_query)) => {
                            let query = cli_query
                                .user_query
                                .as_ref()
                                .map(|query| query.query.clone())
                                .unwrap_or_default();
                            let running_command = cli_query.running_command.as_ref();
                            texts
                                .push(monitor::query_with_running_command(&query, running_command));
                            queries.push(query);
                            // The user can write before the client reports the command's first
                            // snapshot; its snapshot then answers the command's tool call.
                            let call_id = &cli_query.run_shell_command_tool_call_id;
                            if !call_id.is_empty()
                                && !has_tool_response(messages, call_id)
                                && let Some(command) = running_command
                                && let Some(snapshot) = &command.snapshot
                            {
                                tool_responses = tool_responses.with_tool_response(
                                    call_id.clone(),
                                    Ok(tool_result(tools::still_running_text(snapshot), false)),
                                );
                                answered.insert(call_id.clone());
                                let owner = owner_task(tasks, call_id)
                                    .unwrap_or_else(|| self.root_task_id.clone());
                                result_echoes.push(self.new_message_in(
                                    &owner,
                                    Kind::ToolCallResult(ToolCallResult {
                                        tool_call_id: call_id.clone(),
                                        result: Some(
                                            api::message::tool_call_result::Result::RunShellCommand(
                                                api::RunShellCommandResult {
                                                    command: command.command.clone(),
                                                    result: Some(
                                                        api::run_shell_command_result::Result::LongRunningCommandSnapshot(
                                                            snapshot.clone(),
                                                        ),
                                                    ),
                                                    ..Default::default()
                                                },
                                            ),
                                        ),
                                        ..Default::default()
                                    }),
                                ));
                            }
                            if let Some(command) = running_command
                                && let Some(snapshot) = &command.snapshot
                                && !snapshot.command_id.is_empty()
                            {
                                running =
                                    Some((snapshot.command_id.clone(), command.command.clone()));
                                asked_about_running = true;
                            }
                        }
                        Some(UserInput::ToolCallResult(result)) => {
                            let message_result = result.result.clone().map(to_message_result);
                            let (text, is_error) = message_result
                                .as_ref()
                                .map(tools::result_text)
                                .unwrap_or_else(|| ("No result.".to_owned(), true));
                            tool_responses = tool_responses.with_tool_response(
                                result.tool_call_id.clone(),
                                Ok(tool_result(text, is_error)),
                            );
                            answered.insert(result.tool_call_id.clone());
                            match message_result.as_ref().and_then(monitor::command_status) {
                                Some((command_id, true)) => {
                                    finished.insert(command_id);
                                }
                                Some((command_id, false)) if !asked_about_running => {
                                    let command = message_result
                                        .as_ref()
                                        .map(monitor::command_line)
                                        .unwrap_or_default();
                                    running = Some((command_id, command));
                                }
                                Some(_) | None => {}
                            }
                            let owner = owner_task(tasks, &result.tool_call_id)
                                .unwrap_or_else(|| self.task_id.clone());
                            result_echoes.push(self.new_message_in(
                                &owner,
                                Kind::ToolCallResult(ToolCallResult {
                                    tool_call_id: result.tool_call_id.clone(),
                                    result: message_result,
                                    ..Default::default()
                                }),
                            ));
                        }
                        other => {
                            log::debug!("Ignoring unsupported user input: {other:?}");
                        }
                    }
                }
            }
            Some(Type::ResumeConversation(_)) => {
                if let Some(cli) = &self.cli
                    && let Some(command) = executed
                        .iter()
                        .find(|command| command.command_id == cli.command_id)
                {
                    texts.push(monitor::resumed_text(command));
                }
            }
            other => {
                return Err(internal(format!(
                    "this server doesn't support {} requests yet",
                    input_type_name(other.as_ref())
                )));
            }
        }
        if !queries.is_empty() {
            answer_open_tool_calls(messages, &answered, tasks, &mut result_echoes, self);
        }
        self.add_messages_in_order(result_echoes);

        if self
            .cli
            .as_ref()
            .is_some_and(|cli| finished.contains(&cli.command_id))
        {
            self.close_cli_subagent("The command finished.");
        } else if addresses_main_agent || asked_about_running {
            self.close_cli_subagent("The user moved on.");
        }
        if let Some((command_id, command)) = running
            && !finished.contains(&command_id)
        {
            self.open_cli_subagent(command_id, &command);
        }
        self.task_id = self.current_task_id();

        let echoes: Vec<api::Message> = queries
            .into_iter()
            .map(|query| {
                self.new_message(Kind::UserQuery(UserQuery {
                    query,
                    ..Default::default()
                }))
            })
            .collect();
        if !echoes.is_empty() {
            self.actions(vec![self.add_messages(echoes)]);
        }
        if !tool_responses.content.is_empty() {
            messages.push(tool_responses);
        }
        if !texts.is_empty() {
            messages.push(Message::user().with_text(texts.join("\n\n")));
        }
        Ok(())
    }

    async fn run_agent(
        &mut self,
        messages: Vec<Message>,
        model: &str,
        endpoint: &ModelEndpoint,
        provider: Arc<dyn Provider>,
        tools: Vec<Tool>,
        system_prompt: String,
    ) -> Result<Vec<Message>, TurnError> {
        let inference =
            InferenceRunner::<TurnSession, Effect>::new(provider, ModelConfig::new(model));
        let cancel = CancellationToken::new();
        let machine = StateMachine::new(
            vec![
                Step::Operation(Arc::new(ClientTools {
                    tools,
                    system_prompt,
                })),
                Step::Inference(Arc::new(inference)),
            ],
            cancel.clone(),
        );
        let runtime = Runtime {
            messages: Mutex::new(messages),
        };

        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let emit = Emitter::new(events_tx, cancel);
        let mut announced_tool_calls = HashSet::new();
        {
            let run = machine.run(&runtime, "turn", &emit);
            tokio::pin!(run);
            let result = loop {
                tokio::select! {
                    result = &mut run => break result,
                    Some(event) = events_rx.recv() => self.forward(event, &mut announced_tool_calls),
                }
            };
            result.map_err(classify_error)?;
        }
        while let Ok(event) = events_rx.try_recv() {
            self.forward(event, &mut announced_tool_calls);
        }

        let messages = runtime
            .messages
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(error) = trailing_error(&messages) {
            log::warn!("The model call failed: {error}");
            return Err(classify_message_error(&error, model, endpoint));
        }
        Ok(messages)
    }

    /// Streams one agent event to the client: text as it arrives, tool calls once complete.
    fn forward(&mut self, event: AgentEvent, announced_tool_calls: &mut HashSet<String>) {
        let AgentEvent::Message(message) = event else {
            return;
        };
        if message.role != Role::Assistant || !message.metadata.user_visible {
            return;
        }
        for block in &message.content {
            match block {
                MessageContentBlock::Text(text) => self.stream_text(&text.text),
                MessageContentBlock::ToolRequest(request) => {
                    if !announced_tool_calls.insert(request.id.clone()) {
                        continue;
                    }
                    let tool = request
                        .tool_call
                        .as_ref()
                        .map_err(|error| error.message.to_string())
                        .and_then(|call| {
                            let arguments = call.arguments.as_ref();
                            match self.mcp_tools.to_warp_tool(&call.name, arguments) {
                                Some(tool) => Ok(tool),
                                None => tools::to_warp_tool(
                                    &call.name,
                                    arguments,
                                    self.cli.as_ref().map(|cli| cli.command_id.as_str()),
                                ),
                            }
                        });
                    match tool {
                        Ok(tool) => self.tool_call(request.id.clone(), tool),
                        Err(error) => log::warn!("Dropping an unusable tool call: {error}"),
                    }
                }
                _ => {}
            }
        }
    }
}

/// The text of the turn's final assistant reply when the turn ended on an answer, not on tool
/// calls, and the answer carries no `<spoken>` summary.
fn unspoken_final_reply(messages: &[Message]) -> Option<String> {
    let last = messages.last()?;
    let waits_on_tools = last
        .content
        .iter()
        .any(|block| matches!(block, MessageContentBlock::ToolRequest(_)));
    if last.role != Role::Assistant || waits_on_tools {
        return None;
    }
    let text = last.as_concat_text();
    (!text.trim().is_empty() && !text.contains("<spoken>")).then_some(text)
}

/// Restores the model conversation saved in the task's `server_data`, or rebuilds a text-only
/// one from the task's messages when there is none.
fn restore_conversation(task: &api::Task) -> Vec<Message> {
    if !task.server_data.is_empty() {
        match serde_json::from_str::<Vec<Message>>(&task.server_data) {
            Ok(messages) => return messages,
            Err(error) => {
                log::warn!("Rebuilding a conversation whose saved state is unreadable: {error}")
            }
        }
    }
    task.messages
        .iter()
        .filter_map(|message| match &message.message {
            Some(Kind::UserQuery(query)) => Some(Message::user().with_text(query.query.clone())),
            Some(Kind::AgentOutput(output)) if !output.text.is_empty() => {
                Some(Message::assistant().with_text(output.text.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Gives every tool call the client never answered (the user cancelled or rejected it, or
/// interrupted a wait with a follow-up) a cancelled result, since the model API rejects a
/// conversation with unanswered calls. `answered` holds the calls this request answers.
fn answer_open_tool_calls(
    messages: &mut Vec<Message>,
    answered: &HashSet<String>,
    tasks: &[api::Task],
    echoes: &mut Vec<api::Message>,
    stream: &Stream,
) {
    let answered: HashSet<&str> = messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            MessageContentBlock::ToolResponse(response) => Some(response.id.as_str()),
            _ => None,
        })
        .chain(answered.iter().map(String::as_str))
        .collect();
    let open: Vec<String> = messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            MessageContentBlock::ToolRequest(request)
                if !answered.contains(request.id.as_str()) =>
            {
                Some(request.id.clone())
            }
            _ => None,
        })
        .collect();
    if open.is_empty() {
        return;
    }
    let mut responses = Message::user();
    for id in open {
        responses = responses.with_tool_response(
            id.clone(),
            Ok(tool_result(CANCELLED_TOOL_RESULT.to_owned(), true)),
        );
        let owner = owner_task(tasks, &id).unwrap_or_else(|| stream.task_id.clone());
        echoes.push(stream.new_message_in(
            &owner,
            Kind::ToolCallResult(ToolCallResult {
                tool_call_id: id,
                result: Some(api::message::tool_call_result::Result::Cancel(())),
                ..Default::default()
            }),
        ));
    }
    messages.push(responses);
}

fn has_tool_response(messages: &[Message], tool_call_id: &str) -> bool {
    messages
        .iter()
        .flat_map(|message| &message.content)
        .any(|block| matches!(block, MessageContentBlock::ToolResponse(response) if response.id == tool_call_id))
}

/// The task holding the tool call with this id.
fn owner_task(tasks: &[api::Task], tool_call_id: &str) -> Option<String> {
    tasks
        .iter()
        .find(|task| {
            task.messages.iter().any(|message| {
                matches!(&message.message, Some(Kind::ToolCall(call)) if call.tool_call_id == tool_call_id)
            })
        })
        .map(|task| task.id.clone())
}

fn tool_result(text: String, is_error: bool) -> CallToolResult {
    let content = vec![ContentBlock::text(text)];
    if is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    }
}

/// Request and message tool results wrap the same payload types in different enums.
fn to_message_result(
    result: api::request::input::tool_call_result::Result,
) -> api::message::tool_call_result::Result {
    use api::message::tool_call_result::Result as M;
    use api::request::input::tool_call_result::Result as R;
    match result {
        R::RunShellCommand(result) => M::RunShellCommand(result),
        R::ReadFiles(result) => M::ReadFiles(result),
        R::SearchCodebase(result) => M::SearchCodebase(result),
        R::ApplyFileDiffs(result) => M::ApplyFileDiffs(result),
        R::SuggestPlan(result) => M::SuggestPlan(result),
        R::SuggestCreatePlan(result) => M::SuggestCreatePlan(result),
        R::Grep(result) => M::Grep(result),
        #[allow(deprecated)]
        R::FileGlob(result) => M::FileGlob(result),
        R::ReadMcpResource(result) => M::ReadMcpResource(result),
        R::CallMcpTool(result) => M::CallMcpTool(result),
        R::WriteToLongRunningShellCommand(result) => M::WriteToLongRunningShellCommand(result),
        R::SuggestNewConversation(result) => M::SuggestNewConversation(result),
        R::FileGlobV2(result) => M::FileGlobV2(result),
        R::SuggestPrompt(result) => M::SuggestPrompt(result),
        R::OpenCodeReview(result) => M::OpenCodeReview(result),
        R::InitProject(result) => M::InitProject(result),
        R::ReadDocuments(result) => M::ReadDocuments(result),
        R::EditDocuments(result) => M::EditDocuments(result),
        R::CreateDocuments(result) => M::CreateDocuments(result),
        R::ReadShellCommandOutput(result) => M::ReadShellCommandOutput(result),
        R::UseComputer(result) => M::UseComputer(result),
        R::InsertReviewComments(result) => M::InsertReviewComments(result),
        R::RequestComputerUse(result) => M::RequestComputerUseResult(result),
        R::ReadSkill(result) => M::ReadSkill(result),
        R::FetchConversation(result) => M::FetchConversation(result),
        R::SendMessageToAgent(result) => M::SendMessageToAgent(result),
        R::TransferShellCommandControlToUser(result) => {
            M::TransferShellCommandControlToUser(result)
        }
        R::AskUserQuestion(result) => M::AskUserQuestion(result),
        R::UploadFileArtifact(result) => M::UploadFileArtifact(result),
        R::RunAgentsResult(result) => M::RunAgentsResult(result),
        R::WaitForEvents(result) => M::WaitForEvents(result),
        R::StartRecording(result) => M::StartRecording(result),
        R::StopRecording(result) => M::StopRecording(result),
        R::DeniedByHook(result) => M::DeniedByHook(result),
    }
}

fn is_root(task: &api::Task) -> bool {
    task.dependencies
        .as_ref()
        .is_none_or(|dependencies| dependencies.parent_task_id.is_empty())
}

fn input_type_name(input: Option<&api::request::input::Type>) -> String {
    match input {
        Some(other) => format!("{other:?}")
            .split('(')
            .next()
            .unwrap_or_default()
            .to_owned(),
        None => "empty".to_owned(),
    }
}

/// Finds where the request's selected model runs: a custom provider that lists it, or else
/// Anthropic.
pub(super) fn resolve_model(
    settings: Option<&api::request::Settings>,
) -> Result<(ModelEndpoint, String), TurnError> {
    let selected = settings
        .and_then(|settings| settings.model_config.as_ref())
        .map(|config| config.base.clone())
        .filter(|model| !model.is_empty())
        .ok_or_else(|| internal("the request names no model"))?;

    let custom = settings
        .and_then(|settings| settings.custom_model_providers.as_ref())
        .into_iter()
        .flat_map(|registry| &registry.providers)
        .find_map(|provider| {
            provider
                .models
                .iter()
                .find(|model| model.config_key == selected)
                .map(|model| (provider, model))
        });
    if let Some((provider, model)) = custom {
        let schema = CustomEndpointSchema::try_from(provider.schema)
            .unwrap_or(CustomEndpointSchema::OpenaiChatCompletions);
        let api_key = provider.api_key.trim();
        if api_key.is_empty() {
            return Err(TurnError::InvalidApiKey {
                provider: api::LlmProvider::Unknown,
                model: model.slug.clone(),
            });
        }
        let base_url = provider.base_url.trim().to_owned();
        let api_key = api_key.to_owned();
        let endpoint = match schema {
            CustomEndpointSchema::OpenaiChatCompletions => {
                ModelEndpoint::OpenAiChatCompletions { base_url, api_key }
            }
            CustomEndpointSchema::OpenaiResponses => {
                ModelEndpoint::OpenAiResponses { base_url, api_key }
            }
            CustomEndpointSchema::AnthropicMessages => {
                return Err(internal(format!(
                    "this server can't call {} endpoints yet",
                    schema.as_str_name()
                )));
            }
        };
        return Ok((endpoint, model.slug.clone()));
    }

    if !selected.starts_with("claude-") {
        return Err(internal(format!(
            "{selected} is neither a Claude model nor one of your custom models"
        )));
    }
    let credential = settings
        .and_then(|settings| settings.api_keys.as_ref())
        .map(|keys| keys.anthropic.trim().to_owned())
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            log::warn!("The request carries no Anthropic credential");
            TurnError::InvalidApiKey {
                provider: api::LlmProvider::Anthropic,
                model: selected.clone(),
            }
        })?;
    Ok((ModelEndpoint::Anthropic { credential }, selected))
}

pub(super) fn provider_for(endpoint: &ModelEndpoint) -> Result<Arc<dyn Provider>> {
    match endpoint {
        ModelEndpoint::Anthropic { credential } => anthropic_provider(credential),
        ModelEndpoint::OpenAiChatCompletions { base_url, api_key } => {
            let api_client = ApiClient::new_with_tls(
                base_url.clone(),
                AuthMethod::BearerToken(api_key.clone()),
                None,
            )?;
            let provider: Arc<dyn Provider> = Arc::new(OpenAiCompatibleProvider::new(
                endpoint.display_name(),
                api_client,
                String::new(),
            ));
            Ok(if rejects_replayed_reasoning(base_url) {
                Arc::new(WithoutReplayedReasoning(provider))
            } else {
                provider
            })
        }
        ModelEndpoint::OpenAiResponses { base_url, api_key } => Ok(Arc::new(
            ResponsesProvider::new(endpoint.display_name(), base_url, api_key)?,
        )),
    }
}

/// Whether an endpoint rejects the model's earlier reasoning when it comes back in the history,
/// as Cerebras does with `reasoning_content`. Others, such as DeepSeek and Kimi, require it.
fn rejects_replayed_reasoning(base_url: &str) -> bool {
    url::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(|host| host == "api.cerebras.ai"))
        .unwrap_or(false)
}

/// Sends the conversation without the model's earlier reasoning.
struct WithoutReplayedReasoning(Arc<dyn Provider>);

#[async_trait]
impl Provider for WithoutReplayedReasoning {
    fn get_name(&self) -> &str {
        self.0.get_name()
    }

    async fn stream(
        &self,
        model_config: &ModelConfig,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        let messages = without_reasoning(messages);
        self.0.stream(model_config, system, &messages, tools).await
    }
}

fn without_reasoning(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|message| {
            let mut message = message.clone();
            message.content.retain(|block| {
                !matches!(
                    block,
                    MessageContentBlock::Thinking(_) | MessageContentBlock::RedactedThinking(_)
                )
            });
            (!message.content.is_empty()).then_some(message)
        })
        .collect()
}

fn anthropic_provider(credential: &str) -> Result<Arc<dyn Provider>> {
    let api_client = if credential.starts_with(OAUTH_TOKEN_PREFIX) {
        ApiClient::new_with_tls(
            ANTHROPIC_API_URL.to_owned(),
            AuthMethod::BearerToken(credential.to_owned()),
            None,
        )?
        .with_header("anthropic-beta", "oauth-2025-04-20")?
    } else {
        ApiClient::new_with_tls(
            ANTHROPIC_API_URL.to_owned(),
            AuthMethod::ApiKey {
                header_name: "x-api-key".to_owned(),
                key: credential.to_owned(),
            },
            None,
        )?
    }
    .with_header("anthropic-version", "2023-06-01")?;
    Ok(Arc::new(AnthropicProviderBuilder::new(api_client).build()))
}

fn trailing_error(messages: &[Message]) -> Option<String> {
    messages.last().and_then(|message| {
        message.content.iter().find_map(|block| match block {
            MessageContentBlock::Error(error) => Some(error.message.clone()),
            _ => None,
        })
    })
}

pub(super) fn classify_message_error(
    error: &str,
    model: &str,
    endpoint: &ModelEndpoint,
) -> TurnError {
    let lowered = error.to_lowercase();
    if lowered.contains("status: 401") || lowered.contains("invalid x-api-key") {
        TurnError::InvalidApiKey {
            provider: endpoint.llm_provider(),
            model: model.to_owned(),
        }
    } else if lowered.contains("status: 529") || lowered.contains("overloaded") {
        TurnError::Unavailable
    } else {
        TurnError::Internal(format!(
            "{} refused the request: {}",
            endpoint.display_name(),
            provider_reason(error)
        ))
    }
}

/// The provider's own explanation from an error, without the transport details around it.
fn provider_reason(error: &str) -> &str {
    let reason = error
        .split_once("Response: ")
        .map_or(error, |(_, response)| response);
    reason
        .split(" (provider request id")
        .next()
        .unwrap_or(reason)
        .trim()
}

fn classify_error(error: anyhow::Error) -> TurnError {
    TurnError::Internal(format!("{error:#}"))
}

fn internal(message: impl Into<String>) -> TurnError {
    TurnError::Internal(message.into())
}

struct TurnSession {
    conversation: Conversation,
}

impl MachineSession for TurnSession {
    fn id(&self) -> &str {
        "turn"
    }
    fn conversation(&self) -> Option<&Conversation> {
        Some(&self.conversation)
    }
}

enum Effect {
    Message(Box<Message>),
    Usage,
}

impl From<Message> for Effect {
    fn from(message: Message) -> Self {
        Self::Message(Box::new(message))
    }
}

impl MachineEffect for Effect {
    fn ensure_message_ids(&mut self) {
        if let Self::Message(message) = self {
            message.id.get_or_insert_with(|| Uuid::new_v4().to_string());
        }
    }
}

impl InferenceEffect for Effect {
    fn record_usage(_usage: ProviderUsage) -> Self {
        Self::Usage
    }
}

/// Holds the conversation for the duration of one turn.
struct Runtime {
    messages: Mutex<Vec<Message>>,
}

#[async_trait]
impl SessionLoader<TurnSession> for Runtime {
    async fn load(&self, _session_id: &str) -> Result<TurnSession> {
        let messages = self
            .messages
            .lock()
            .map_err(|_| anyhow!("conversation lock poisoned"))?;
        Ok(TurnSession {
            conversation: Conversation::new_unvalidated(messages.clone()),
        })
    }
}

#[async_trait]
impl EffectHandler<TurnSession, Effect> for Runtime {
    async fn apply_effects(
        &self,
        _session: &TurnSession,
        effects: &mut [Effect],
        _emit: &Emitter,
    ) -> Result<()> {
        let mut messages = self
            .messages
            .lock()
            .map_err(|_| anyhow!("conversation lock poisoned"))?;
        for effect in effects.iter() {
            match effect {
                Effect::Message(message) => messages.push((**message).clone()),
                Effect::Usage => {}
            }
        }
        Ok(())
    }
}

/// Advertises the client-executed tools and the system prompt. It never runs a tool: once the
/// model asks for one, no step applies and the machine yields, ending the turn so the client
/// can run it.
struct ClientTools {
    tools: Vec<Tool>,
    system_prompt: String,
}

#[async_trait]
impl Operation<TurnSession, Effect> for ClientTools {
    fn name(&self) -> &'static str {
        "client_tools"
    }

    async fn inference_tools(&self, _session: &TurnSession) -> Result<Vec<Tool>> {
        Ok(self.tools.clone())
    }

    async fn prompt_parts(
        &self,
        _session: &TurnSession,
        _conversation: &Conversation,
    ) -> Result<Vec<(String, String)>> {
        Ok(vec![("system".to_owned(), self.system_prompt.clone())])
    }

    async fn run(
        &self,
        _session: &TurnSession,
        _conversation: &Conversation,
        _emit: &Emitter,
    ) -> Result<OperationResult<Effect>> {
        not_applicable()
    }
}

#[cfg(test)]
#[path = "turn_tests.rs"]
mod tests;
