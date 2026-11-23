//! Long-running commands. The client gives up waiting for a command after a few seconds and
//! returns a snapshot of its output instead. Like Warp's server, this one then hands the command
//! to a CLI subagent: a subtask of the root task, announced by a `Subagent` tool call in the root
//! task and closed by that call's result. While it is open, the client shows the command as
//! monitored by the agent (with take over and hide responses), routes follow-ups to the subtask,
//! and runs the subtask's `ReadShellCommandOutput` and `WriteToLongRunningShellCommand` calls.

use uuid::Uuid;
use warp_multi_agent_api as api;
use warp_multi_agent_api::message::tool_call::{self as call, subagent};
use warp_multi_agent_api::message::{Message as Kind, ToolCall, ToolCallResult, tool_call_result};

/// The most snapshot output put into a prompt; the client sends up to 1000 lines.
const MAX_SNAPSHOT_BYTES: usize = 30 * 1024;

/// An open CLI subagent subtask and the command it monitors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CliSubtask {
    pub task_id: String,
    /// The id of the root task's `Subagent` tool call that opened the subtask.
    pub call_id: String,
    /// The client's id for the command's block.
    pub command_id: String,
}

impl CliSubtask {
    pub fn new(command_id: String) -> Self {
        Self {
            task_id: Uuid::new_v4().to_string(),
            call_id: format!("cli-{}", Uuid::new_v4()),
            command_id,
        }
    }

    /// The latest CLI subagent the root task opened and hasn't closed.
    pub fn open_in(root: &api::Task) -> Option<Self> {
        let closed: Vec<&str> = root
            .messages
            .iter()
            .filter_map(|message| match &message.message {
                Some(Kind::ToolCallResult(result)) => Some(result.tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        root.messages
            .iter()
            .rev()
            .find_map(|message| match &message.message {
                Some(Kind::ToolCall(ToolCall {
                    tool_call_id,
                    tool:
                        Some(call::Tool::Subagent(call::Subagent {
                            task_id,
                            metadata: Some(subagent::Metadata::Cli(cli)),
                            ..
                        })),
                })) if !closed.contains(&tool_call_id.as_str()) => Some(Self {
                    task_id: task_id.clone(),
                    call_id: tool_call_id.clone(),
                    command_id: cli.command_id.clone(),
                }),
                _ => None,
            })
    }

    /// The root task's tool call that opens this subagent.
    pub fn opening_call(&self, command: &str) -> Kind {
        Kind::ToolCall(ToolCall {
            tool_call_id: self.call_id.clone(),
            tool: Some(call::Tool::Subagent(call::Subagent {
                task_id: self.task_id.clone(),
                payload: format!("Monitor the running command: {command}"),
                metadata: Some(subagent::Metadata::Cli(subagent::CliSubagent {
                    command_id: self.command_id.clone(),
                })),
            })),
        })
    }

    /// The subtask, as a child of `root_task_id`.
    pub fn task(&self, root_task_id: &str) -> api::Task {
        api::Task {
            id: self.task_id.clone(),
            dependencies: Some(api::task::Dependencies {
                parent_task_id: root_task_id.to_owned(),
            }),
            ..Default::default()
        }
    }

    /// The result of the opening call, which closes the subagent.
    pub fn closing_result(&self, summary: &str) -> Kind {
        Kind::ToolCallResult(ToolCallResult {
            tool_call_id: self.call_id.clone(),
            result: Some(tool_call_result::Result::Subagent(
                tool_call_result::SubagentResult {
                    payload: summary.to_owned(),
                },
            )),
            ..Default::default()
        })
    }
}

/// The command id and whether the command finished, for a shell tool result that reports one.
pub(super) fn command_status(result: &tool_call_result::Result) -> Option<(String, bool)> {
    use api::read_shell_command_output_result::Result as Read;
    use api::run_shell_command_result::Result as Run;
    use api::write_to_long_running_shell_command_result::Result as Write;
    use tool_call_result::Result as R;
    match result {
        R::RunShellCommand(result) => match &result.result {
            Some(Run::LongRunningCommandSnapshot(snapshot)) => {
                Some((snapshot.command_id.clone(), false))
            }
            Some(Run::CommandFinished(finished)) => Some((finished.command_id.clone(), true)),
            _ => None,
        },
        R::ReadShellCommandOutput(result) => match &result.result {
            Some(Read::LongRunningCommandSnapshot(snapshot)) => {
                Some((snapshot.command_id.clone(), false))
            }
            Some(Read::CommandFinished(finished)) => Some((finished.command_id.clone(), true)),
            _ => None,
        },
        R::WriteToLongRunningShellCommand(result) => match &result.result {
            Some(Write::LongRunningCommandSnapshot(snapshot)) => {
                Some((snapshot.command_id.clone(), false))
            }
            Some(Write::CommandFinished(finished)) => Some((finished.command_id.clone(), true)),
            _ => None,
        },
        _ => None,
    }
    .filter(|(command_id, _)| !command_id.is_empty())
}

/// The command line a shell tool result reports, if any.
pub(super) fn command_line(result: &tool_call_result::Result) -> String {
    match result {
        tool_call_result::Result::RunShellCommand(result) => result.command.clone(),
        tool_call_result::Result::ReadShellCommandOutput(result) => result.command.clone(),
        _ => String::new(),
    }
}

/// A follow-up the user sent while a command runs, with the command's current output.
pub(super) fn query_with_running_command(
    query: &str,
    running: Option<&api::RunningShellCommand>,
) -> String {
    let Some(running) = running else {
        return query.to_owned();
    };
    let snapshot = running.snapshot.clone().unwrap_or_default();
    format!(
        "{query}\n\n[The command `{}` is still running in the terminal (command_id: {}). Its \
         current output:\n{}]",
        running.command,
        snapshot.command_id,
        truncate_start(&snapshot.output)
    )
}

/// The note added when control of the monitored command comes back to the agent.
pub(super) fn resumed_text(command: &api::ExecutedShellCommand) -> String {
    if command.finished_ts.is_some() {
        format!(
            "[The command `{}` finished with exit code {}. Output:\n{}]",
            command.command,
            command.exit_code,
            truncate_start(&command.output)
        )
    } else {
        format!(
            "[The user handed control of the running command `{}` back to you (command_id: {}). \
             Its current output:\n{}]",
            command.command,
            command.command_id,
            truncate_start(&command.output)
        )
    }
}

/// System prompt guidance while the agent works in a CLI subagent.
pub(super) fn instructions(command_id: &str) -> String {
    format!(
        "\n\n# Monitoring a running command\nA command you started is still running in the \
         user's terminal (command_id: {command_id}). Answer the user's messages about it, and \
         keep the user informed. Use read_shell_command_output to check its output or to wait \
         for it to finish, write_to_shell_command to answer its prompts, and stop_shell_command \
         to interrupt it with Ctrl-C when the user asks you to stop it or it is clearly stuck. \
         You can't run other commands until it exits. When you have nothing to do until the \
         user replies, end your turn."
    )
}

/// Keeps the end of long output, where a running command's latest lines are.
fn truncate_start(output: &str) -> &str {
    if output.len() <= MAX_SNAPSHOT_BYTES {
        return output;
    }
    let mut start = output.len() - MAX_SNAPSHOT_BYTES;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    &output[start..]
}

#[cfg(test)]
#[path = "monitor_tests.rs"]
mod tests;
