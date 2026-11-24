//! Prompt suggestions: after a shell command or an agent response, the user's model proposes one
//! follow-up prompt for the agent, or declines.

use std::sync::Arc;
use std::time::SystemTime;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use uuid::Uuid;
use warp_multi_agent_api as api;
use warp_multi_agent_api::client_action::{self, CreateTask};
use warp_multi_agent_api::message::tool_call::suggest_prompt::{DisplayMode, PromptChip};
use warp_multi_agent_api::message::tool_call::{SuggestPrompt, Tool};
use warp_multi_agent_api::message::{Message as Kind, ToolCall};
use warp_multi_agent_api::request::input::generate_passive_suggestions::Trigger;

use super::helpers::{complete, json_span, tail};
use super::turn::{ProviderFactory, TurnError};
use crate::{AppState, bearer_token};

const OUTPUT_TAIL_CHARS: usize = 4_000;
const AGENT_EXCHANGE_CHARS: usize = 2_000;
const MAX_PROMPT_CHARS: usize = 300;
const MAX_LABEL_CHARS: usize = 60;

const SYSTEM_PROMPT: &str = "You suggest the next request a developer might give their coding \
agent, based on what just happened in their terminal. Suggest only when there is a clear, \
useful next step the agent could take, such as fixing a failure, explaining an error or \
continuing unfinished work. Do not suggest for routine successful commands, and never restate \
what the user just did. Reply with exactly one JSON object, {\"prompt\": \"<the request, \
written as the user, at most two sentences>\", \"label\": \"<a 2 to 6 word chip label>\"}, or \
with the single word NONE.";

/// A suggested prompt and its optional short chip label.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Suggestion {
    pub prompt: String,
    pub label: String,
}

/// Answers one passive-suggestions request on `out`: `Init`, a `SuggestPrompt` chip if the model
/// offers one, then `Finished(Done)`. Failures are logged and end the stream without a chip.
pub(super) async fn suggest(
    request: api::Request,
    out: mpsc::UnboundedSender<api::ResponseEvent>,
    make_provider: ProviderFactory,
) {
    let conversation_id = request
        .metadata
        .as_ref()
        .map(|metadata| metadata.conversation_id.clone())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let request_id = Uuid::new_v4().to_string();
    let send = |event| {
        let _ = out.send(api::ResponseEvent {
            r#type: Some(event),
        });
    };
    send(api::response_event::Type::Init(
        api::response_event::StreamInit {
            conversation_id,
            request_id: request_id.clone(),
            ..Default::default()
        },
    ));

    if let Some(context) = trigger_context(&request)
        && let Some(settings) = &request.settings
    {
        match complete(settings, SYSTEM_PROMPT, context, &make_provider).await {
            Ok(reply) => {
                if let Some(suggestion) = parse_suggestion(&reply) {
                    send(api::response_event::Type::ClientActions(
                        api::response_event::ClientActions {
                            actions: vec![api::ClientAction {
                                action: Some(suggestion_task(suggestion, &request_id)),
                            }],
                        },
                    ));
                }
            }
            Err(error) => log_failure(&error),
        }
    }

    send(api::response_event::Type::Finished(
        api::response_event::StreamFinished {
            reason: Some(api::response_event::stream_finished::Reason::Done(
                Default::default(),
            )),
            ..Default::default()
        },
    ));
}

/// What the model should base its suggestion on, or `None` for triggers it can't help with.
fn trigger_context(request: &api::Request) -> Option<String> {
    let input = request.input.as_ref()?;
    let Some(api::request::input::Type::GeneratePassiveSuggestions(passive)) = &input.r#type else {
        return None;
    };
    match passive.trigger.as_ref()? {
        Trigger::ShellCommandCompleted(completed) => {
            let command = completed.executed_shell_command.as_ref()?;
            Some(shell_command_context(
                &command.command,
                command.exit_code,
                &command.output,
            ))
        }
        Trigger::AgentResponseCompleted(_) => agent_exchange_context(request),
        Trigger::FilesChanged(()) | Trigger::CommandRun(()) => None,
    }
}

pub(super) fn shell_command_context(command: &str, exit_code: i32, output: &str) -> String {
    format!(
        "The user ran this shell command:\n{command}\n\nExit code: {exit_code}\n\nOutput (tail):\n{}",
        tail(output, OUTPUT_TAIL_CHARS)
    )
}

fn agent_exchange_context(request: &api::Request) -> Option<String> {
    let messages = request
        .task_context
        .as_ref()?
        .tasks
        .iter()
        .flat_map(|task| &task.messages);
    let mut last_query = None;
    let mut last_output = None;
    for message in messages {
        match &message.message {
            Some(Kind::UserQuery(query)) if !query.query.is_empty() => {
                last_query = Some(query.query.as_str())
            }
            Some(Kind::AgentOutput(output)) if !output.text.is_empty() => {
                last_output = Some(output.text.as_str())
            }
            _ => {}
        }
    }
    let output = last_output?;
    Some(format!(
        "The user asked their agent:\n{}\n\nThe agent answered (tail):\n{}",
        tail(
            last_query.unwrap_or("(not available)"),
            AGENT_EXCHANGE_CHARS
        ),
        tail(output, AGENT_EXCHANGE_CHARS)
    ))
}

#[derive(Deserialize)]
struct SuggestionReply {
    prompt: String,
    #[serde(default)]
    label: String,
}

/// The suggestion in a model reply, or `None` when the model declined or answered unusably.
pub(super) fn parse_suggestion(reply: &str) -> Option<Suggestion> {
    let reply: SuggestionReply = serde_json::from_str(json_span(reply, '{', '}')?).ok()?;
    let prompt = reply.prompt.trim();
    if prompt.is_empty() || prompt.chars().count() > MAX_PROMPT_CHARS {
        return None;
    }
    Some(Suggestion {
        prompt: prompt.to_owned(),
        label: reply.label.trim().chars().take(MAX_LABEL_CHARS).collect(),
    })
}

fn suggestion_task(suggestion: Suggestion, request_id: &str) -> client_action::Action {
    let task_id = Uuid::new_v4().to_string();
    let message = api::Message {
        id: Uuid::new_v4().to_string(),
        task_id: task_id.clone(),
        request_id: request_id.to_owned(),
        timestamp: Some(prost_types::Timestamp::from(SystemTime::now())),
        message: Some(Kind::ToolCall(ToolCall {
            tool_call_id: Uuid::new_v4().to_string(),
            tool: Some(Tool::SuggestPrompt(SuggestPrompt {
                display_mode: Some(DisplayMode::PromptChip(PromptChip {
                    prompt: suggestion.prompt,
                    label: suggestion.label,
                })),
                is_trigger_irrelevant: false,
            })),
        })),
        ..Default::default()
    };
    client_action::Action::CreateTask(CreateTask {
        task: Some(api::Task {
            id: task_id,
            messages: vec![message],
            ..Default::default()
        }),
    })
}

fn log_failure(error: &TurnError) {
    match error {
        TurnError::InvalidApiKey { model, .. } => {
            log::info!("No prompt suggestion: no usable credential for {model}")
        }
        TurnError::Unavailable => log::info!("No prompt suggestion: the model is unavailable"),
        TurnError::Internal(message) => log::warn!("No prompt suggestion: {message}"),
    }
}

/// The legacy `/ai/generate_am_query_suggestions` request.
#[derive(Deserialize)]
pub struct LegacySuggestionRequest {
    #[serde(default)]
    context_messages: Vec<String>,
    #[serde(default)]
    system_context: Option<String>,
    #[serde(default)]
    exit_code: i32,
}

/// `POST /ai/generate_am_query_suggestions`. The request carries no credentials, so this uses the
/// user's remembered model settings and answers `suggestion: null` without them.
pub async fn generate_am_query_suggestions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<LegacySuggestionRequest>,
) -> Response {
    let Some(user) =
        bearer_token(&headers).and_then(|token| state.sessions.user_for_id_token(token))
    else {
        return (StatusCode::UNAUTHORIZED, "sign in required").into_response();
    };
    let suggestion = match state.ai_settings.latest(&user.uid) {
        Some(settings) => legacy_suggestion(&settings, &request, &state.make_provider).await,
        None => {
            log::info!("No prompt suggestion: no model settings seen yet this session");
            None
        }
    };
    Json(json!({
        "id": Uuid::new_v4().to_string(),
        "suggestion": suggestion.map(|suggestion| json!({
            "simple": { "query": suggestion.prompt, "should_plan_task": false },
        })),
    }))
    .into_response()
}

pub(super) async fn legacy_suggestion(
    settings: &api::request::Settings,
    request: &LegacySuggestionRequest,
    make_provider: &ProviderFactory,
) -> Option<Suggestion> {
    let mut context = format!(
        "Recent terminal blocks (tail):\n{}\n\nLast exit code: {}",
        tail(&request.context_messages.join("\n\n"), OUTPUT_TAIL_CHARS),
        request.exit_code
    );
    if let Some(system) = request.system_context.as_deref().filter(|s| !s.is_empty()) {
        context.push_str(&format!("\n\nSystem: {system}"));
    }
    match complete(settings, SYSTEM_PROMPT, context, make_provider).await {
        Ok(reply) => parse_suggestion(&reply),
        Err(error) => {
            log_failure(&error);
            None
        }
    }
}

#[cfg(test)]
#[path = "suggestions_tests.rs"]
mod tests;
