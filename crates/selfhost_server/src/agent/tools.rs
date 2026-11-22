//! The tools offered to the model. Each one is executed by the Warp client, not the server: a
//! model tool call becomes a Warp `ToolCall` message, and the client's result comes back in the
//! next request as a `ToolCallResult`, which becomes the model's tool result.

use std::sync::Arc;

use rmcp::model::Tool;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use warp_multi_agent_api as api;
use warp_multi_agent_api::message::tool_call::{self as call};

/// A client tool the server can offer, and the Warp tool types that must be supported for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentTool {
    RunShellCommand,
    ReadFiles,
    EditFile,
    WriteFile,
    Grep,
    FindFiles,
}

impl AgentTool {
    const ALL: [AgentTool; 6] = [
        AgentTool::RunShellCommand,
        AgentTool::ReadFiles,
        AgentTool::EditFile,
        AgentTool::WriteFile,
        AgentTool::Grep,
        AgentTool::FindFiles,
    ];

    fn name(self) -> &'static str {
        match self {
            AgentTool::RunShellCommand => "run_shell_command",
            AgentTool::ReadFiles => "read_files",
            AgentTool::EditFile => "edit_file",
            AgentTool::WriteFile => "write_file",
            AgentTool::Grep => "grep",
            AgentTool::FindFiles => "find_files",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.name() == name)
    }

    fn requires(self) -> api::ToolType {
        match self {
            AgentTool::RunShellCommand => api::ToolType::RunShellCommand,
            AgentTool::ReadFiles => api::ToolType::ReadFiles,
            AgentTool::EditFile | AgentTool::WriteFile => api::ToolType::ApplyFileDiffs,
            AgentTool::Grep => api::ToolType::Grep,
            AgentTool::FindFiles => api::ToolType::FileGlobV2,
        }
    }

    fn description(self) -> &'static str {
        match self {
            AgentTool::RunShellCommand => {
                "Run a shell command in the user's terminal and return its output and exit code. \
                 Set read_only to true only for commands that change nothing (ls, cat, git \
                 status, tests that write no files); set risky to true for commands that delete \
                 data, rewrite history, or reach outside the machine. The user may be asked to \
                 approve commands that are not read-only."
            }
            AgentTool::ReadFiles => {
                "Read one or more files. Optionally limit each file to a 1-based inclusive line \
                 range. Prefer this over cat."
            }
            AgentTool::EditFile => {
                "Replace an exact block of text in an existing file. old_text must match the \
                 file's current content, including whitespace; include enough surrounding lines \
                 to make it unique. Read the file first."
            }
            AgentTool::WriteFile => {
                "Create a file, or replace a file's entire content. Prefer edit_file for changes \
                 to existing files."
            }
            AgentTool::Grep => {
                "Search file contents for one or more regular expressions under a directory. \
                 Returns matching files and line numbers; use read_files to see the lines."
            }
            AgentTool::FindFiles => {
                "Find files whose paths match glob patterns (for example **/*.rs) under a \
                 directory."
            }
        }
    }

    fn input_schema(self) -> Value {
        match self {
            AgentTool::RunShellCommand => json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command line to run." },
                    "read_only": { "type": "boolean", "description": "True if the command changes nothing." },
                    "risky": { "type": "boolean", "description": "True if the command is destructive or reaches outside the machine." }
                },
                "required": ["command", "read_only", "risky"]
            }),
            AgentTool::ReadFiles => json!({
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" },
                                "start_line": { "type": "integer", "minimum": 1 },
                                "end_line": { "type": "integer", "minimum": 1 }
                            },
                            "required": ["path"]
                        }
                    }
                },
                "required": ["files"]
            }),
            AgentTool::EditFile => json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_text": { "type": "string" },
                    "new_text": { "type": "string" }
                },
                "required": ["path", "old_text", "new_text"]
            }),
            AgentTool::WriteFile => json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
            AgentTool::Grep => json!({
                "type": "object",
                "properties": {
                    "patterns": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                    "path": { "type": "string", "description": "Directory to search; defaults to the working directory." }
                },
                "required": ["patterns"]
            }),
            AgentTool::FindFiles => json!({
                "type": "object",
                "properties": {
                    "patterns": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                    "path": { "type": "string", "description": "Directory to search; defaults to the working directory." }
                },
                "required": ["patterns"]
            }),
        }
    }

    fn definition(self) -> Tool {
        let Value::Object(schema) = self.input_schema() else {
            unreachable!("tool schemas are JSON objects");
        };
        Tool::new(self.name(), self.description(), Arc::new(schema))
    }
}

/// The model-facing definitions of the tools the client supports in this session.
pub fn definitions(supported: &[i32]) -> Vec<Tool> {
    AgentTool::ALL
        .into_iter()
        .filter(|tool| supported.contains(&(tool.requires() as i32)))
        .map(AgentTool::definition)
        .collect()
}

#[derive(Deserialize)]
struct RunShellCommandArgs {
    command: String,
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    risky: bool,
}

#[derive(Deserialize)]
struct ReadFilesArgs {
    files: Vec<ReadFileArg>,
}

#[derive(Deserialize)]
struct ReadFileArg {
    path: String,
    start_line: Option<u32>,
    end_line: Option<u32>,
}

#[derive(Deserialize)]
struct EditFileArgs {
    path: String,
    old_text: String,
    new_text: String,
}

#[derive(Deserialize)]
struct WriteFileArgs {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct SearchArgs {
    patterns: Vec<String>,
    #[serde(default)]
    path: Option<String>,
}

/// Converts a model tool call into the Warp tool the client executes.
pub fn to_warp_tool(
    name: &str,
    arguments: Option<&Map<String, Value>>,
) -> Result<call::Tool, String> {
    let tool =
        AgentTool::from_name(name).ok_or_else(|| format!("Tool '{name}' is not available."))?;
    let arguments = Value::Object(arguments.cloned().unwrap_or_default());
    let parse_error = |error: serde_json::Error| format!("Invalid arguments for {name}: {error}");
    Ok(match tool {
        AgentTool::RunShellCommand => {
            let args: RunShellCommandArgs =
                serde_json::from_value(arguments).map_err(parse_error)?;
            call::Tool::RunShellCommand(call::RunShellCommand {
                command: args.command,
                is_read_only: args.read_only,
                is_risky: args.risky,
                ..Default::default()
            })
        }
        AgentTool::ReadFiles => {
            let args: ReadFilesArgs = serde_json::from_value(arguments).map_err(parse_error)?;
            call::Tool::ReadFiles(call::ReadFiles {
                files: args
                    .files
                    .into_iter()
                    .map(|file| call::read_files::File {
                        name: file.path,
                        line_ranges: match (file.start_line, file.end_line) {
                            // The client treats `end` as exclusive.
                            (Some(start), Some(end)) => vec![api::FileContentLineRange {
                                start,
                                end: end.saturating_add(1),
                            }],
                            (Some(start), None) => vec![api::FileContentLineRange {
                                start,
                                end: u32::MAX,
                            }],
                            (None, _) => Vec::new(),
                        },
                    })
                    .collect(),
            })
        }
        AgentTool::EditFile => {
            let args: EditFileArgs = serde_json::from_value(arguments).map_err(parse_error)?;
            call::Tool::ApplyFileDiffs(call::ApplyFileDiffs {
                summary: format!("Edit {}", args.path),
                diffs: vec![call::apply_file_diffs::FileDiff {
                    file_path: args.path,
                    search: args.old_text,
                    replace: args.new_text,
                }],
                ..Default::default()
            })
        }
        AgentTool::WriteFile => {
            let args: WriteFileArgs = serde_json::from_value(arguments).map_err(parse_error)?;
            call::Tool::ApplyFileDiffs(call::ApplyFileDiffs {
                summary: format!("Write {}", args.path),
                new_files: vec![call::apply_file_diffs::NewFile {
                    file_path: args.path,
                    content: args.content,
                    allow_overwrite: true,
                }],
                ..Default::default()
            })
        }
        AgentTool::Grep => {
            let args: SearchArgs = serde_json::from_value(arguments).map_err(parse_error)?;
            if args.patterns.is_empty() {
                return Err("grep needs at least one pattern".to_owned());
            }
            call::Tool::Grep(call::Grep {
                queries: args.patterns,
                path: args.path.unwrap_or_default(),
            })
        }
        AgentTool::FindFiles => {
            let args: SearchArgs = serde_json::from_value(arguments).map_err(parse_error)?;
            call::Tool::FileGlobV2(call::FileGlobV2 {
                patterns: with_top_level_patterns(args.patterns),
                search_dir: args.path.unwrap_or_default(),
                ..Default::default()
            })
        }
    })
}

/// The client's glob requires `**/` to match at least one directory, so `**/*.rs` would skip
/// files directly in the search directory. Add each such pattern's top-level form.
fn with_top_level_patterns(patterns: Vec<String>) -> Vec<String> {
    let mut expanded = Vec::with_capacity(patterns.len() * 2);
    for pattern in patterns {
        let top_level = pattern.strip_prefix("**/").map(str::to_owned);
        expanded.push(pattern);
        if let Some(top_level) = top_level
            && !top_level.is_empty()
            && !expanded.contains(&top_level)
        {
            expanded.push(top_level);
        }
    }
    expanded
}

/// Renders a client tool result as text for the model, and whether it reports a failure.
/// The most tool output passed to the model at once; larger output keeps its start and end.
const MAX_RESULT_BYTES: usize = 100 * 1024;

/// The text the model sees for a tool's result, and whether the tool failed.
pub fn result_text(result: &api::message::tool_call_result::Result) -> (String, bool) {
    let (text, failed) = full_result_text(result);
    (truncate_middle(text, MAX_RESULT_BYTES), failed)
}

fn truncate_middle(text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let half = max_bytes / 2;
    let mut head_end = half;
    while !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len() - half;
    while !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}\n\n[... {} bytes of output omitted ...]\n\n{}",
        &text[..head_end],
        tail_start - head_end,
        &text[tail_start..]
    )
}

fn full_result_text(result: &api::message::tool_call_result::Result) -> (String, bool) {
    use api::message::tool_call_result::Result as R;
    match result {
        R::RunShellCommand(result) => shell_result_text(result),
        R::ReadFiles(result) => read_files_text(result),
        R::ApplyFileDiffs(result) => apply_diffs_text(result),
        R::Grep(result) => grep_text(result),
        R::FileGlobV2(result) => glob_text(result),
        R::CallMcpTool(result) => super::mcp::result_text(result),
        R::Cancel(()) => ("The user cancelled this tool call.".to_owned(), true),
        other => (format!("{other:?}"), false),
    }
}

fn shell_result_text(result: &api::RunShellCommandResult) -> (String, bool) {
    use api::run_shell_command_result::Result as R;
    match &result.result {
        Some(R::CommandFinished(finished)) => (
            format!("Exit code {}\n{}", finished.exit_code, finished.output),
            finished.exit_code != 0,
        ),
        Some(R::LongRunningCommandSnapshot(snapshot)) => (
            format!(
                "The command is still running. Output so far:\n{}",
                snapshot.output
            ),
            false,
        ),
        Some(R::PermissionDenied(_)) => (
            "The command was not run: it is denied by the user's command policy.".to_owned(),
            true,
        ),
        Some(R::TerminalBusy(_)) => (
            "The command was not run: the terminal is busy with another command.".to_owned(),
            true,
        ),
        None => ("The command produced no result.".to_owned(), true),
    }
}

fn read_files_text(result: &api::ReadFilesResult) -> (String, bool) {
    use api::read_files_result::Result as R;
    match &result.result {
        Some(R::AnyFilesSuccess(success)) => {
            let mut text = String::new();
            for file in &success.files {
                match &file.content {
                    Some(api::any_file_content::Content::TextContent(content)) => {
                        let range = content
                            .line_range
                            .as_ref()
                            .map(|range| {
                                format!(" (lines {}-{})", range.start, range.end.saturating_sub(1))
                            })
                            .unwrap_or_default();
                        text.push_str(&format!(
                            "=== {}{range}\n{}\n",
                            content.file_path, content.content
                        ));
                    }
                    Some(api::any_file_content::Content::BinaryContent(content)) => {
                        text.push_str(&format!(
                            "=== {} (binary file, not shown)\n",
                            content.file_path
                        ));
                    }
                    None => {}
                }
            }
            for failed in &success.failed_reads {
                text.push_str(&format!(
                    "=== {}: could not read: {}\n",
                    failed.path, failed.message
                ));
            }
            (text, false)
        }
        Some(R::Error(error)) => (error.message.clone(), true),
        other => (format!("{other:?}"), false),
    }
}

fn apply_diffs_text(result: &api::ApplyFileDiffsResult) -> (String, bool) {
    use api::apply_file_diffs_result::Result as R;
    match &result.result {
        Some(R::Success(success)) => {
            let mut paths: Vec<String> = success
                .updated_files_v2
                .iter()
                .filter_map(|file| file.file.as_ref().map(|content| content.file_path.clone()))
                .collect();
            paths.extend(
                success
                    .deleted_files
                    .iter()
                    .map(|file| file.file_path.clone()),
            );
            let edited_by_user = success
                .updated_files_v2
                .iter()
                .any(|file| file.was_edited_by_user);
            let note = if edited_by_user {
                " The user edited the change before accepting it; read the file again before further edits."
            } else {
                ""
            };
            (
                format!("Applied changes to {}.{note}", paths.join(", ")),
                false,
            )
        }
        Some(R::Error(error)) => (error.message.clone(), true),
        None => ("No result.".to_owned(), true),
    }
}

fn grep_text(result: &api::GrepResult) -> (String, bool) {
    use api::grep_result::Result as R;
    match &result.result {
        Some(R::Success(success)) => {
            if success.matched_files.is_empty() {
                return ("No matches.".to_owned(), false);
            }
            let lines = success
                .matched_files
                .iter()
                .map(|file| {
                    let numbers = file
                        .matched_lines
                        .iter()
                        .map(|line| line.line_number.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{}: lines {numbers}", file.file_path)
                })
                .collect::<Vec<_>>();
            (lines.join("\n"), false)
        }
        Some(R::Error(error)) => (error.message.clone(), true),
        None => ("No result.".to_owned(), true),
    }
}

fn glob_text(result: &api::FileGlobV2Result) -> (String, bool) {
    use api::file_glob_v2_result::Result as R;
    match &result.result {
        Some(R::Success(success)) => {
            if success.matched_files.is_empty() {
                return ("No files matched.".to_owned(), false);
            }
            let files = success
                .matched_files
                .iter()
                .map(|file| file.file_path.clone())
                .collect::<Vec<_>>();
            (files.join("\n"), false)
        }
        Some(R::Error(error)) => (error.message.clone(), true),
        None => ("No result.".to_owned(), true),
    }
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
