use serde_json::json;

use super::*;

fn to_warp_tool(name: &str, arguments: Option<&Map<String, Value>>) -> Result<call::Tool, String> {
    super::to_warp_tool(name, arguments, None)
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("arguments are an object"),
    }
}

#[test]
fn only_tools_the_client_supports_are_offered() {
    let names = |supported: &[api::ToolType]| {
        definitions(
            &supported
                .iter()
                .map(|tool| *tool as i32)
                .collect::<Vec<_>>(),
        )
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&[api::ToolType::RunShellCommand]),
        vec!["run_shell_command"]
    );
    assert_eq!(
        names(&[api::ToolType::ApplyFileDiffs, api::ToolType::Grep]),
        vec!["edit_file", "write_file", "grep"]
    );
    assert!(names(&[]).is_empty());
}

#[test]
fn read_file_ranges_become_the_clients_end_exclusive_ranges() {
    let tool = to_warp_tool(
        "read_files",
        Some(&args(json!({ "files": [
            { "path": "a.rs", "start_line": 3, "end_line": 5 },
            { "path": "b.rs" }
        ] }))),
    )
    .unwrap();
    let call::Tool::ReadFiles(read) = tool else {
        panic!("expected ReadFiles")
    };
    assert_eq!(
        read.files[0].line_ranges,
        vec![api::FileContentLineRange { start: 3, end: 6 }]
    );
    assert!(read.files[1].line_ranges.is_empty());
}

#[test]
fn edits_and_writes_become_file_diffs() {
    let call::Tool::ApplyFileDiffs(edit) = to_warp_tool(
        "edit_file",
        Some(&args(
            json!({ "path": "a.rs", "old_text": "x", "new_text": "y" }),
        )),
    )
    .unwrap() else {
        panic!("expected ApplyFileDiffs")
    };
    assert_eq!(edit.diffs[0].search, "x");
    assert_eq!(edit.diffs[0].replace, "y");

    let call::Tool::ApplyFileDiffs(write) = to_warp_tool(
        "write_file",
        Some(&args(
            json!({ "path": "new.rs", "content": "fn main() {}" }),
        )),
    )
    .unwrap() else {
        panic!("expected ApplyFileDiffs")
    };
    assert!(write.new_files[0].allow_overwrite);
    assert_eq!(write.new_files[0].content, "fn main() {}");
}

#[test]
fn bad_calls_are_rejected_with_a_reason() {
    assert!(
        to_warp_tool("delete_everything", None)
            .unwrap_err()
            .contains("not available")
    );
    assert!(to_warp_tool("run_shell_command", Some(&args(json!({})))).is_err());
    assert!(to_warp_tool("grep", Some(&args(json!({ "patterns": [] })))).is_err());
}

#[test]
fn results_render_as_text_for_the_model() {
    use api::message::tool_call_result::Result as R;
    let (text, failed) = result_text(&R::RunShellCommand(api::RunShellCommandResult {
        result: Some(api::run_shell_command_result::Result::CommandFinished(
            api::ShellCommandFinished {
                output: "boom".to_owned(),
                exit_code: 2,
                ..Default::default()
            },
        )),
        ..Default::default()
    }));
    assert_eq!(text, "Exit code 2\nboom");
    assert!(failed);

    let (text, failed) = result_text(&R::Cancel(()));
    assert!(text.contains("cancelled"));
    assert!(failed);
}

#[test]
fn recursive_globs_also_match_the_top_level() {
    let call::Tool::FileGlobV2(glob) = to_warp_tool(
        "find_files",
        Some(&args(
            json!({ "patterns": ["**/*", "**/*.rs", "src/*.toml"] }),
        )),
    )
    .unwrap() else {
        panic!("expected FileGlobV2")
    };
    assert_eq!(
        glob.patterns,
        vec!["**/*", "*", "**/*.rs", "*.rs", "src/*.toml"]
    );
}

#[test]
fn long_results_keep_their_start_and_end() {
    let text = format!("{}{}", "a".repeat(80_000), "é".repeat(60_000));
    let truncated = truncate_middle(text, 1000);
    assert!(truncated.len() < 1100);
    assert!(truncated.starts_with("aaa"));
    assert!(truncated.ends_with("é"));
    assert!(truncated.contains("bytes of output omitted"));
}

#[test]
fn short_results_are_unchanged() {
    assert_eq!(truncate_middle("ok".to_owned(), 1000), "ok");
}

#[test]
fn command_tools_address_the_monitored_command_by_default() {
    use call::read_shell_command_output::Delay;
    use call::write_to_long_running_shell_command::mode::Mode;

    let call::Tool::ReadShellCommandOutput(read) = super::to_warp_tool(
        "read_shell_command_output",
        Some(&args(json!({ "until_complete": true }))),
        Some("block-1"),
    )
    .unwrap() else {
        panic!("expected ReadShellCommandOutput")
    };
    assert_eq!(read.command_id, "block-1");
    assert_eq!(read.delay, Some(Delay::OnCompletion(())));

    let call::Tool::ReadShellCommandOutput(read) = super::to_warp_tool(
        "read_shell_command_output",
        Some(&args(
            json!({ "command_id": "block-2", "wait_seconds": 500 }),
        )),
        Some("block-1"),
    )
    .unwrap() else {
        panic!("expected ReadShellCommandOutput")
    };
    assert_eq!(read.command_id, "block-2");
    assert_eq!(
        read.delay,
        Some(Delay::Duration(prost_types::Duration {
            seconds: 120,
            nanos: 0
        }))
    );

    let call::Tool::WriteToLongRunningShellCommand(write) = super::to_warp_tool(
        "write_to_shell_command",
        Some(&args(json!({ "input": "y" }))),
        Some("block-1"),
    )
    .unwrap() else {
        panic!("expected WriteToLongRunningShellCommand")
    };
    assert_eq!(write.command_id, "block-1");
    assert_eq!(write.input, b"y");
    assert_eq!(write.mode.unwrap().mode, Some(Mode::Line(())));

    let call::Tool::WriteToLongRunningShellCommand(stop) =
        super::to_warp_tool("stop_shell_command", None, Some("block-1")).unwrap()
    else {
        panic!("expected WriteToLongRunningShellCommand")
    };
    assert_eq!(stop.command_id, "block-1");
    assert_eq!(stop.input, b"\x03");
    assert_eq!(stop.mode.unwrap().mode, Some(Mode::Raw(())));
}

#[test]
fn command_tools_need_a_command() {
    assert!(
        to_warp_tool("stop_shell_command", None)
            .unwrap_err()
            .contains("command_id")
    );
}

#[test]
fn command_tools_are_offered_when_the_client_supports_them() {
    let names: Vec<String> = definitions(&[
        api::ToolType::ReadShellCommandOutput as i32,
        api::ToolType::WriteToLongRunningShellCommand as i32,
    ])
    .into_iter()
    .map(|tool| tool.name.to_string())
    .collect();
    assert_eq!(
        names,
        vec![
            "read_shell_command_output",
            "write_to_shell_command",
            "stop_shell_command"
        ]
    );
}

#[test]
fn running_command_results_tell_the_model_how_to_continue() {
    use api::message::tool_call_result::Result as R;
    let snapshot = api::LongRunningShellCommandSnapshot {
        output: "partial".to_owned(),
        command_id: "block-1".to_owned(),
        is_preempted: true,
        ..Default::default()
    };
    let (text, failed) = result_text(&R::RunShellCommand(api::RunShellCommandResult {
        result: Some(
            api::run_shell_command_result::Result::LongRunningCommandSnapshot(snapshot.clone()),
        ),
        ..Default::default()
    }));
    assert!(!failed);
    assert!(text.contains("still running (command_id: block-1)"));
    assert!(text.contains("partial"));
    assert!(text.contains("before the command exited"));
    assert!(text.contains("stop_shell_command"));

    let (text, failed) = result_text(&R::ReadShellCommandOutput(
        api::ReadShellCommandOutputResult {
            result: Some(
                api::read_shell_command_output_result::Result::CommandFinished(
                    api::ShellCommandFinished {
                        output: "^C".to_owned(),
                        exit_code: 130,
                        ..Default::default()
                    },
                ),
            ),
            ..Default::default()
        },
    ));
    assert!(failed);
    assert!(text.contains("exit code 130"));

    let (text, failed) = result_text(&R::WriteToLongRunningShellCommand(
        api::WriteToLongRunningShellCommandResult {
            result: Some(
                api::write_to_long_running_shell_command_result::Result::Error(
                    api::ShellCommandError {
                        r#type: Some(api::shell_command_error::Type::CommandNotFound(())),
                    },
                ),
            ),
        },
    ));
    assert!(failed);
    assert!(text.contains("No running command"));
}
