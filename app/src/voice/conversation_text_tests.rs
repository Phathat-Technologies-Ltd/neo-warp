use super::*;
use crate::ai::agent::{AIAgentOutputMessage, AIAgentText, MessageId};

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

fn reply(markdown: &str) -> AIAgentOutput {
    let text = AIAgentText::from(warp_multi_agent_api::message::AgentOutput {
        text: markdown.to_owned(),
    });
    AIAgentOutput {
        messages: vec![AIAgentOutputMessage {
            id: MessageId::new("m".to_owned()),
            message: AIAgentOutputMessageType::Text(text),
            citations: Vec::new(),
        }],
        ..Default::default()
    }
}

#[test]
fn speaks_only_the_spoken_block_and_hides_it_from_the_screen() {
    let output = reply(
        "<spoken>I fixed the failing test.</spoken>\n\nEdited `app/src/lib.rs` and ran `cargo test`.",
    );
    assert_eq!(
        spoken_reply_text(&output, true),
        "I fixed the failing test."
    );
    let AIAgentOutputMessageType::Text(text) = &output.messages[0].message else {
        panic!("expected a text message");
    };
    let shown = text
        .sections
        .iter()
        .filter_map(|section| match section {
            AIAgentTextSection::PlainText { text } => Some(text.text().to_owned()),
            AIAgentTextSection::Code { .. }
            | AIAgentTextSection::Table { .. }
            | AIAgentTextSection::Image { .. }
            | AIAgentTextSection::MermaidDiagram { .. } => None,
        })
        .collect::<String>();
    assert!(!shown.contains("spoken"), "{shown}");
    assert!(shown.contains("cargo test"), "{shown}");
}

#[test]
fn replies_without_a_spoken_block_use_the_filtered_readout() {
    let output = reply(
        "Updated `crates/voice_input/src/live.rs` so capture recovers.\n\n```rust\nfn x() {}\n```\n\n\
         Run cargo test -p voice_input next. Then check the logs.",
    );
    assert_eq!(
        spoken_reply_text(&output, true),
        "Updated so capture recovers. Run cargo test next."
    );
}
