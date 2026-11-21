use serde_json::json;

use super::*;

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
