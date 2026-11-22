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
