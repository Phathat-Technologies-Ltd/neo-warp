use super::*;

fn command(command: &str, is_risky: Option<bool>) -> AIAgentActionType {
    AIAgentActionType::RequestCommandOutput {
        command: command.to_owned(),
        is_read_only: None,
        is_risky,
        wait_until_completion: true,
        uses_pager: None,
        rationale: None,
        citations: Vec::new(),
    }
}

#[test]
fn summarizes_tool_calls() {
    assert_eq!(
        SpokenAction(&command("cargo check -p app", None)).spoken_summary(),
        Some("Running cargo check.".to_owned())
    );
    assert_eq!(
        SpokenAction(&AIAgentActionType::Grep {
            queries: vec!["fn main".to_owned()],
            path: ".".to_owned(),
        })
        .spoken_summary(),
        Some("Searching the code.".to_owned())
    );
    assert_eq!(
        SpokenAction(&AIAgentActionType::InitProject).spoken_summary(),
        None
    );
}

#[test]
fn approval_prompts_use_the_risk_classification() {
    assert_eq!(
        spoken_action_prompt(&command("cargo test", Some(false))),
        ("cargo test".to_owned(), false)
    );
    assert_eq!(
        spoken_action_prompt(&command("rm -rf target", Some(true))),
        ("rm -rf target".to_owned(), true)
    );
    let (_, needs_screen) = spoken_action_prompt(&AIAgentActionType::InitProject);
    assert!(needs_screen);
}

#[test]
fn unclassified_commands_need_the_screen() {
    assert_eq!(
        spoken_action_prompt(&command("cargo test", None)),
        ("cargo test".to_owned(), true)
    );
}

fn completed(command: &str, exit_code: i32, output: &str) -> AIAgentActionResultType {
    AIAgentActionResultType::RequestCommandOutput(RequestCommandOutputResult::Completed {
        block_id: crate::terminal::model::block::BlockId::new(),
        command: command.to_owned(),
        output: output.to_owned(),
        exit_code: exit_code.into(),
        start_ts: None,
        completed_ts: None,
    })
}

#[test]
fn command_results_are_read_when_short_and_summarized_when_long() {
    let short = spoken_action_result(&completed("pwd", 0, "/tmp\n")).unwrap();
    assert!(short.starts_with("Ran pwd: "), "{short}");
    let long = "warning: x\n".repeat(40) + "error: y\n";
    assert_eq!(
        spoken_action_result(&completed("cargo check -p app", 101, &long)),
        Some("Ran cargo check: 1 error, 40 warnings.".to_owned())
    );
    assert_eq!(
        spoken_action_result(&AIAgentActionResultType::RequestCommandOutput(
            RequestCommandOutputResult::CancelledBeforeExecution
        )),
        None
    );
}
