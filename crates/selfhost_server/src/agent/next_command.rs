//! Next-command suggestions (`POST /ai/generate_input_suggestions`): the shell command the user
//! most likely runs next, or the completion of the command they have started typing.
//!
//! The client asks only when its own history-based prediction isn't confident, and while the user
//! types, so answers use the user's selected model with a short timeout, a debounce for typed
//! prefixes, and a short-lived cache of identical requests. Any failure answers with no
//! suggestion.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use instant::Instant;
use serde::Deserialize;
use serde_json::json;
use warp_multi_agent_api as api;

use super::helpers::{complete, tail};
use super::turn::{ProviderFactory, TurnError, resolve_model};
use crate::{AppState, bearer_token};

/// How long the model may take before the request answers with no suggestion.
const MODEL_TIMEOUT: Duration = Duration::from_millis(2_500);
/// How long a request with a typed prefix waits for the user to stop typing before asking the
/// model; a newer prefix request from the same user in that time supersedes it.
const PREFIX_DEBOUNCE: Duration = Duration::from_millis(150);
const CACHE_TTL: Duration = Duration::from_secs(30);
const CACHE_CAPACITY: usize = 256;
const MAX_COMMAND_CHARS: usize = 300;
const BLOCKS_CHARS: usize = 6_000;
const HISTORY_CHARS: usize = 3_000;
const MAX_REJECTED_SUGGESTIONS: usize = 5;

const SYSTEM_PROMPT: &str = "You predict the next shell command a developer will run in their \
terminal, from their recent terminal blocks, similar sequences from their command history, their \
shell and OS, and the text they have typed so far, if any. Reply with exactly one shell command on \
a single line and nothing else: no explanation, no markdown, no backticks, no quotes around it, no \
prompt symbol. When text has been typed, the command must start with exactly that text, character \
for character, and continue it. Prefer commands the history shows the user running in similar \
situations. Never suggest destructive commands such as recursive deletes, force pushes, hard \
resets, or disk formatting. If you are not confident what comes next, reply with the single word \
NONE.";

/// The `/ai/generate_input_suggestions` request. Field names match the client's
/// `GenerateAIInputSuggestionsRequest`; anything this server doesn't use is ignored.
#[derive(Debug, Default, Deserialize, Hash)]
pub struct NextCommandRequest {
    /// The latest terminal blocks, oldest first, each a JSON object with `input`, `output` and
    /// `context` (`pwd`, `git_branch`, `exit_code`).
    #[serde(default)]
    pub context_messages: Vec<String>,
    /// Similar command sequences from the user's history, one JSON command per line.
    #[serde(default)]
    pub history_context: String,
    /// The shell and OS, as JSON.
    #[serde(default)]
    pub system_context: Option<String>,
    #[serde(default)]
    pub rejected_suggestions: Vec<String>,
    /// What the user has typed; absent right after a command finishes.
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub block_context: Option<FinishedBlock>,
    /// The model the user picked for these suggestions (a model ID such as `claude-haiku-4-5` or
    /// a custom model's config key); absent means the agent's selected model.
    #[serde(default)]
    pub model: Option<String>,
}

/// The parts of the client's `BlockContext` for the command that just finished.
#[derive(Debug, Default, Deserialize, Hash)]
pub struct FinishedBlock {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub pwd: Option<String>,
    #[serde(default)]
    pub git_branch: Option<String>,
}

/// Recent answers and the per-user debounce state, in memory only.
#[derive(Default)]
pub struct NextCommandState {
    cache: Mutex<HashMap<u64, (Instant, String)>>,
    latest_prefix_request: Mutex<HashMap<String, u64>>,
}

impl NextCommandState {
    fn cached(&self, key: u64) -> Option<String> {
        let cache = lock(&self.cache);
        cache
            .get(&key)
            .filter(|(at, _)| at.elapsed() < CACHE_TTL)
            .map(|(_, answer)| answer.clone())
    }

    fn remember(&self, key: u64, answer: &str) {
        let mut cache = lock(&self.cache);
        if cache.len() >= CACHE_CAPACITY {
            cache.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
            if cache.len() >= CACHE_CAPACITY {
                cache.clear();
            }
        }
        cache.insert(key, (Instant::now(), answer.to_owned()));
    }

    /// Records a new prefix request for `uid` and returns its sequence number.
    fn start_prefix_request(&self, uid: &str) -> u64 {
        let mut latest = lock(&self.latest_prefix_request);
        let sequence = latest.get(uid).map_or(1, |sequence| sequence + 1);
        latest.insert(uid.to_owned(), sequence);
        sequence
    }

    fn is_latest_prefix_request(&self, uid: &str, sequence: u64) -> bool {
        lock(&self.latest_prefix_request).get(uid) == Some(&sequence)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `POST /ai/generate_input_suggestions`. The request carries no credentials, so this uses the
/// user's remembered model settings and answers with no suggestion without them.
pub async fn generate_input_suggestions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<NextCommandRequest>,
) -> Response {
    let Some(user) =
        bearer_token(&headers).and_then(|token| state.sessions.user_for_id_token(token))
    else {
        return (StatusCode::UNAUTHORIZED, "sign in required").into_response();
    };
    let command = match state.ai_settings.latest(&user.uid) {
        Some(settings) => {
            suggest_next_command(
                &settings,
                &request,
                &user.uid,
                &state.next_command,
                &state.make_provider,
                MODEL_TIMEOUT,
            )
            .await
        }
        None => {
            log::debug!("No next-command suggestion: no model settings seen yet this session");
            None
        }
    };
    Json(suggestions_response(command)).into_response()
}

/// The client's `GenerateAIInputSuggestionsResponseV2`; no suggestion is an empty
/// `most_likely_action`.
pub(super) fn suggestions_response(command: Option<String>) -> serde_json::Value {
    let command = command.unwrap_or_default();
    json!({
        "commands": if command.is_empty() { vec![] } else { vec![command.clone()] },
        "ai_queries": [],
        "most_likely_action": command,
    })
}

/// The suggested command, or `None` when the model declines, answers unusably, fails, or takes
/// longer than `timeout`.
pub(super) async fn suggest_next_command(
    settings: &api::request::Settings,
    request: &NextCommandRequest,
    uid: &str,
    state: &NextCommandState,
    make_provider: &ProviderFactory,
    timeout: Duration,
) -> Option<String> {
    let prefix = request
        .prefix
        .as_deref()
        .filter(|prefix| !prefix.is_empty());
    if prefix.is_some_and(|prefix| prefix.trim().is_empty()) {
        return None;
    }
    let settings = settings_for_model(settings, request.model.as_deref());
    let key = cache_key(uid, &settings, request);
    if let Some(answer) = state.cached(key) {
        return (!answer.is_empty()).then_some(answer);
    }
    if prefix.is_some() {
        let sequence = state.start_prefix_request(uid);
        tokio::time::sleep(PREFIX_DEBOUNCE).await;
        if !state.is_latest_prefix_request(uid, sequence) {
            return None;
        }
    }
    let reply = match tokio::time::timeout(
        timeout,
        complete(
            &settings,
            SYSTEM_PROMPT,
            user_message(request),
            make_provider,
        ),
    )
    .await
    {
        Ok(Ok(reply)) => reply,
        Ok(Err(error)) => {
            log_failure(&error);
            return None;
        }
        Err(_) => {
            log::info!("No next-command suggestion: the model took longer than {timeout:?}");
            return None;
        }
    };
    let command = sanitize_command(&reply, prefix);
    state.remember(key, command.as_deref().unwrap_or_default());
    match &command {
        Some(command) => log::debug!("Next-command suggestion: {command}"),
        None => log::debug!("No next-command suggestion from the model's reply"),
    }
    command
}

/// `settings` with `model` as the selected model when the settings can reach it (a known model
/// with a credential), else `settings` unchanged so suggestions fall back to the agent's model.
pub(super) fn settings_for_model(
    settings: &api::request::Settings,
    model: Option<&str>,
) -> api::request::Settings {
    let mut chosen = settings.clone();
    let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) else {
        return chosen;
    };
    chosen
        .model_config
        .get_or_insert_with(Default::default)
        .base = model.to_owned();
    match resolve_model(Some(&chosen)) {
        Ok(_) => chosen,
        Err(_) => {
            log::debug!(
                "Next-command model {model} isn't usable with the current credentials; using the \
                 agent's model"
            );
            settings.clone()
        }
    }
}

fn cache_key(uid: &str, settings: &api::request::Settings, request: &NextCommandRequest) -> u64 {
    let mut hasher = DefaultHasher::new();
    uid.hash(&mut hasher);
    settings
        .model_config
        .as_ref()
        .map(|config| config.base.as_str())
        .hash(&mut hasher);
    request.hash(&mut hasher);
    hasher.finish()
}

/// The context the model predicts from.
pub(super) fn user_message(request: &NextCommandRequest) -> String {
    let mut sections = Vec::new();
    if let Some(system) = request
        .system_context
        .as_deref()
        .filter(|system| !system.is_empty())
    {
        sections.push(format!("Shell and OS: {system}"));
    }
    if !request.context_messages.is_empty() {
        sections.push(format!(
            "Recent terminal blocks, oldest first:\n{}",
            tail(&request.context_messages.join("\n"), BLOCKS_CHARS)
        ));
    }
    if let Some(block) = request
        .block_context
        .as_ref()
        .filter(|block| !block.command.is_empty())
    {
        let mut line = format!("The command that just finished: {}", block.command);
        if let Some(exit_code) = block.exit_code {
            line.push_str(&format!("\nExit code: {exit_code}"));
        }
        if let Some(pwd) = block.pwd.as_deref().filter(|pwd| !pwd.is_empty()) {
            line.push_str(&format!("\nWorking directory: {pwd}"));
        }
        if let Some(branch) = block.git_branch.as_deref().filter(|b| !b.is_empty()) {
            line.push_str(&format!("\nGit branch: {branch}"));
        }
        sections.push(line);
    }
    if !request.history_context.trim().is_empty() {
        sections.push(format!(
            "Similar sequences from the user's history, oldest first (\"...\" separates \
             sequences):\n{}",
            tail(&request.history_context, HISTORY_CHARS)
        ));
    }
    let rejected: Vec<_> = request
        .rejected_suggestions
        .iter()
        .filter(|suggestion| !suggestion.is_empty())
        .take(MAX_REJECTED_SUGGESTIONS)
        .map(String::as_str)
        .collect();
    if !rejected.is_empty() {
        sections.push(format!(
            "The user rejected these suggestions, so don't repeat them:\n{}",
            rejected.join("\n")
        ));
    }
    sections.push(
        match request
            .prefix
            .as_deref()
            .filter(|prefix| !prefix.is_empty())
        {
            Some(prefix) => format!(
                "The user has typed (complete it; your command must start with exactly this):\n\
                 {prefix}"
            ),
            None => "The user hasn't typed anything yet.".to_owned(),
        },
    );
    sections.join("\n\n")
}

/// The command in a model reply, or `None` when the model declined or the reply isn't a single,
/// safe command that starts with `prefix`.
pub(super) fn sanitize_command(reply: &str, prefix: Option<&str>) -> Option<String> {
    let mut lines = reply
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("```"));
    let line = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    let command = line.trim_matches('`').trim();
    let command = command.strip_prefix("$ ").unwrap_or(command).trim();
    if command.is_empty()
        || command.eq_ignore_ascii_case("none")
        || command.chars().count() > MAX_COMMAND_CHARS
        || command.chars().any(char::is_control)
    {
        return None;
    }
    if let Some(prefix) = prefix.filter(|prefix| !prefix.is_empty())
        && (!command.starts_with(prefix) || command == prefix)
    {
        return None;
    }
    if is_destructive(command) {
        return None;
    }
    Some(command.to_owned())
}

/// Whether running `command` could destroy data or rewrite shared history. Errs on the side of
/// rejecting: a missed suggestion costs little, an accepted destructive one costs a lot.
pub(super) fn is_destructive(command: &str) -> bool {
    let lowered = command.to_lowercase();
    let compact: String = lowered.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains(":(){") || compact.contains("of=/dev/") || compact.contains(">/dev/sd") {
        return true;
    }
    lowered.split([';', '&', '\n']).any(|list| {
        list.split('|').enumerate().any(|(index, segment)| {
            let words = program_and_args(segment);
            let Some((program, args)) = words.split_first() else {
                return false;
            };
            // Piping into a shell runs code no one has read.
            (index > 0 && matches!(*program, "sh" | "bash" | "zsh" | "fish"))
                || is_destructive_invocation(program, args)
        })
    })
}

/// The words of one pipeline segment, without leading wrappers such as `sudo` or `FOO=bar`.
fn program_and_args(segment: &str) -> Vec<&str> {
    let mut words: Vec<&str> = segment.split_whitespace().collect();
    while let Some(first) = words.first() {
        let is_wrapper = matches!(
            *first,
            "sudo" | "doas" | "command" | "nohup" | "time" | "exec" | "env" | "xargs"
        ) || (first.contains('=') && !first.starts_with('-'));
        if !is_wrapper {
            break;
        }
        words.remove(0);
    }
    words
}

/// Whether any short-option cluster in `args` (such as `-rf`) contains `flag`, or `long` appears.
fn has_flag(args: &[&str], long: &str, flag: char) -> bool {
    args.iter().any(|arg| {
        *arg == long || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains(flag))
    })
}

/// Checks one already-lowercased invocation.
fn is_destructive_invocation(program: &str, args: &[&str]) -> bool {
    match program {
        "rm" => has_flag(args, "--recursive", 'r'),
        "shred" | "wipefs" | "fdisk" | "mkfs" | "shutdown" | "reboot" | "halt" | "poweroff" => true,
        _ if program.starts_with("mkfs.") => true,
        "chmod" | "chown" => {
            has_flag(args, "--recursive", 'r')
                && args
                    .iter()
                    .any(|arg| matches!(*arg, "/" | "/*" | "~" | "~/"))
        }
        "diskutil" => args
            .iter()
            .any(|arg| arg.starts_with("erase") || matches!(*arg, "zerodisk" | "secureerase")),
        "kill" | "pkill" | "killall" => args.contains(&"-1"),
        "git" => is_destructive_git(args),
        "terraform" | "tofu" => args.contains(&"destroy"),
        "kubectl" => args.contains(&"delete"),
        _ => {
            let joined = args.join(" ");
            joined.contains("drop database") || joined.contains("drop table")
        }
    }
}

fn is_destructive_git(args: &[&str]) -> bool {
    let mut words = args.iter().copied();
    let subcommand = loop {
        match words.next() {
            // These global options take a value.
            Some("-c" | "-C" | "--git-dir" | "--work-tree") => {
                words.next();
            }
            Some(option) if option.starts_with('-') => {}
            Some(subcommand) => break subcommand,
            None => return false,
        }
    };
    let rest: Vec<&str> = words.collect();
    match subcommand {
        "push" => {
            has_flag(&rest, "--delete", 'f')
                || has_flag(&rest, "--delete", 'd')
                || rest.iter().any(|arg| {
                    arg.starts_with("--force")
                        || matches!(*arg, "--mirror" | "--prune")
                        || arg.starts_with('+')
                        || arg.starts_with(':')
                })
        }
        "reset" => rest.contains(&"--hard"),
        "clean" => has_flag(&rest, "--force", 'f'),
        // `-D` force-deletes; the command is lowercased, so this also rejects `-d`.
        "branch" => has_flag(&rest, "--delete", 'd'),
        "checkout" | "restore" => rest.contains(&".") && !rest.contains(&"--staged"),
        "filter-branch" | "filter-repo" => true,
        "stash" => rest
            .first()
            .is_some_and(|arg| matches!(*arg, "clear" | "drop")),
        _ => false,
    }
}

fn log_failure(error: &TurnError) {
    match error {
        TurnError::InvalidApiKey { model, .. } => {
            log::info!("No next-command suggestion: no usable credential for {model}")
        }
        TurnError::Unavailable => {
            log::info!("No next-command suggestion: the model is unavailable")
        }
        TurnError::Internal(message) => log::warn!("No next-command suggestion: {message}"),
    }
}

#[cfg(test)]
#[path = "next_command_tests.rs"]
mod tests;
