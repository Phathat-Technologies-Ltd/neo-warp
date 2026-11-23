use super::*;

fn message(kind: Kind) -> api::Message {
    api::Message {
        message: Some(kind),
        ..Default::default()
    }
}

#[test]
fn the_open_subagent_is_found_until_its_call_has_a_result() {
    let cli = CliSubtask::new("block-1".to_owned());
    let mut root = api::Task {
        id: "root".to_owned(),
        messages: vec![message(cli.opening_call("sleep 100"))],
        ..Default::default()
    };
    assert_eq!(CliSubtask::open_in(&root), Some(cli.clone()));

    root.messages.push(message(cli.closing_result("done")));
    assert_eq!(CliSubtask::open_in(&root), None);
}

#[test]
fn the_subtask_is_a_child_of_the_root() {
    let cli = CliSubtask::new("block-1".to_owned());
    let task = cli.task("root");
    assert_eq!(task.id, cli.task_id);
    assert_eq!(task.dependencies.unwrap().parent_task_id, "root");
}

#[test]
fn shell_results_report_their_command_and_whether_it_finished() {
    use api::run_shell_command_result::Result as Run;
    let running = tool_call_result::Result::RunShellCommand(api::RunShellCommandResult {
        command: "find /".to_owned(),
        result: Some(Run::LongRunningCommandSnapshot(
            api::LongRunningShellCommandSnapshot {
                command_id: "block-1".to_owned(),
                ..Default::default()
            },
        )),
        ..Default::default()
    });
    assert_eq!(
        command_status(&running),
        Some(("block-1".to_owned(), false))
    );
    assert_eq!(command_line(&running), "find /");

    let finished =
        tool_call_result::Result::ReadShellCommandOutput(api::ReadShellCommandOutputResult {
            result: Some(
                api::read_shell_command_output_result::Result::CommandFinished(
                    api::ShellCommandFinished {
                        command_id: "block-1".to_owned(),
                        ..Default::default()
                    },
                ),
            ),
            ..Default::default()
        });
    assert_eq!(
        command_status(&finished),
        Some(("block-1".to_owned(), true))
    );
    assert_eq!(command_status(&tool_call_result::Result::Cancel(())), None);
}

#[test]
fn follow_ups_carry_the_end_of_the_running_output() {
    let output = format!("{}LATEST", "x".repeat(MAX_SNAPSHOT_BYTES));
    let text = query_with_running_command(
        "Have you stopped it?",
        Some(&api::RunningShellCommand {
            command: "find /".to_owned(),
            snapshot: Some(api::LongRunningShellCommandSnapshot {
                output,
                command_id: "block-1".to_owned(),
                ..Default::default()
            }),
        }),
    );
    assert!(text.starts_with("Have you stopped it?"));
    assert!(text.contains("`find /` is still running"));
    assert!(text.contains("command_id: block-1"));
    assert!(text.contains("LATEST"));
    assert!(text.len() < MAX_SNAPSHOT_BYTES + 300);
}
