//! AI command search (the GraphQL `GenerateCommands` mutation): the user's model turns a
//! description into shell commands with fillable `{{parameter}}` placeholders.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use warp_multi_agent_api as api;

use super::helpers::{complete, json_span};
use super::turn::{ProviderFactory, TurnError};
use crate::AppState;
use crate::sessions::User;

const MAX_COMMANDS: usize = 3;

const SYSTEM_PROMPT: &str = "You turn a description of a task into shell commands. Reply with \
only a JSON array of at most 3 objects, best first: [{\"command\": \"<one shell command>\", \
\"description\": \"<what it does, one short sentence>\", \"parameters\": [{\"id\": \"<name>\", \
\"description\": \"<what to fill in>\"}]}]. When the command needs a value the user hasn't given \
(a file, a host, a branch), write it as {{name}} in the command and list it in parameters. Use \
the user's operating system's tools. Reply [] if the request isn't something a shell command \
can do.";

/// Why command search failed, as the GraphQL `GenerateCommandsFailureType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    AiProviderError,
    BadPrompt,
    RateLimited,
}

impl Failure {
    fn graphql_name(self) -> &'static str {
        match self {
            Self::AiProviderError => "AI_PROVIDER_ERROR",
            Self::BadPrompt => "BAD_PROMPT",
            Self::RateLimited => "RATE_LIMITED",
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct GeneratedCommand {
    pub command: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Parameter {
    pub id: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GraphqlBody {
    #[serde(default)]
    variables: Variables,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Variables {
    #[serde(default)]
    input: Input,
    #[serde(default)]
    request_context: Option<RequestContext>,
}

#[derive(Deserialize, Default)]
struct Input {
    #[serde(default)]
    prompt: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestContext {
    #[serde(default)]
    os_context: Option<OsContext>,
}

#[derive(Deserialize)]
struct OsContext {
    name: Option<String>,
    version: Option<String>,
}

/// The `GenerateCommands` response for a GraphQL request `body` from `user`.
pub async fn generate_commands(state: &AppState, user: &User, body: &[u8]) -> Value {
    let body: GraphqlBody = serde_json::from_slice(body).unwrap_or_default();
    let os = body
        .variables
        .request_context
        .and_then(|context| context.os_context)
        .map(|os| {
            [os.name, os.version]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|os| !os.is_empty());
    let result = match state.ai_settings.latest(&user.uid) {
        Some(settings) => {
            search(
                &settings,
                &body.variables.input.prompt,
                os.as_deref(),
                &state.make_provider,
            )
            .await
        }
        None => {
            log::info!("Command search refused: no model settings seen yet this session");
            Err(Failure::AiProviderError)
        }
    };
    response(result)
}

pub(super) async fn search(
    settings: &api::request::Settings,
    prompt: &str,
    os: Option<&str>,
    make_provider: &ProviderFactory,
) -> Result<Vec<GeneratedCommand>, Failure> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err(Failure::BadPrompt);
    }
    let user = match os {
        Some(os) => format!("Operating system: {os}\n\nTask: {prompt}"),
        None => format!("Task: {prompt}"),
    };
    let reply = complete(settings, SYSTEM_PROMPT, user, make_provider)
        .await
        .map_err(|error| match error {
            TurnError::InvalidApiKey { model, .. } => {
                log::info!("Command search refused: no usable credential for {model}");
                Failure::AiProviderError
            }
            TurnError::Unavailable => Failure::RateLimited,
            TurnError::Internal(message) => {
                log::warn!("Command search failed: {message}");
                Failure::AiProviderError
            }
        })?;
    let commands = parse_commands(&reply);
    if commands.is_empty() {
        return Err(Failure::BadPrompt);
    }
    Ok(commands)
}

/// The usable commands in a model reply: non-empty, at most [`MAX_COMMANDS`], and with only the
/// parameters their command references.
pub(super) fn parse_commands(reply: &str) -> Vec<GeneratedCommand> {
    let Some(commands) = json_span(reply, '[', ']')
        .and_then(|span| serde_json::from_str::<Vec<GeneratedCommand>>(span).ok())
    else {
        return Vec::new();
    };
    commands
        .into_iter()
        .filter_map(|mut command| {
            command.command = command.command.trim().to_owned();
            if command.command.is_empty() {
                return None;
            }
            let text = command.command.clone();
            command
                .parameters
                .retain(|parameter| text.contains(&format!("{{{{{}}}}}", parameter.id)));
            Some(command)
        })
        .take(MAX_COMMANDS)
        .collect()
}

fn response(result: Result<Vec<GeneratedCommand>, Failure>) -> Value {
    let status = match result {
        Ok(commands) => json!({
            "__typename": "GenerateCommandsSuccess",
            "commands": commands,
        }),
        Err(failure) => json!({
            "__typename": "GenerateCommandsFailure",
            "type": failure.graphql_name(),
        }),
    };
    json!({
        "data": {
            "generateCommands": {
                "__typename": "GenerateCommandsOutput",
                "status": status,
                "responseContext": { "serverVersion": null },
            },
        },
    })
}

#[cfg(test)]
#[path = "command_search_tests.rs"]
mod tests;
