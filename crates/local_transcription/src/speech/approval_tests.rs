use std::time::Duration;

use instant::Instant;

use super::*;

#[test]
fn matches_approval_phrases() {
    for text in [
        "Yes.",
        "yes",
        "Run it.",
        "approve",
        "Yes, run it!",
        "Please approve.",
    ] {
        assert_eq!(match_approval(text), Some(ApprovalReply::Approve), "{text}");
    }
    for text in ["No.", "skip", "Cancel.", "No thanks"] {
        assert_eq!(match_approval(text), Some(ApprovalReply::Reject), "{text}");
    }
    for text in [
        "",
        "yes but first check the logs",
        "no wait, run cargo test instead",
        "what does it do?",
    ] {
        assert_eq!(match_approval(text), None, "{text}");
    }
}

#[test]
fn recognizes_stop() {
    assert!(is_stop_phrase("Stop."));
    assert!(is_stop_phrase("stop talking"));
    assert!(!is_stop_phrase("stop the server"));
}

#[test]
fn prompt_reads_a_short_command() {
    assert_eq!(
        approval_prompt("cargo test --workspace", false),
        "I want to run cargo test dash dash workspace. Say run it, or no."
    );
    assert_eq!(
        approval_prompt("rm -rf target", true),
        "I want to run rm dash r f target. Please approve it on screen."
    );
    assert_eq!(
        approval_prompt("a b c d e f g h", false),
        "I want to run a b c d e f and so on. Say run it, or no."
    );
}

#[test]
fn vague_affirmatives_are_not_answers() {
    for text in ["yeah", "yep", "sure", "ok"] {
        assert_eq!(match_approval(text), None, "{text}");
    }
}

#[test]
fn flow_answers_only_matching_utterances_after_the_prompt() {
    let start = Instant::now();
    let mut flow = ApprovalFlow::default();
    assert_eq!(
        flow.on_utterance("yes", Some(start), start),
        ApprovalVerdict::NotAnAnswer
    );
    flow.request(7, "ls", false);
    // The prompt has not played yet.
    assert_eq!(
        flow.on_utterance("yes", Some(start), start),
        ApprovalVerdict::Ignored
    );
    flow.prompt_played(start);
    let listening = start + ECHO_TAIL;
    assert_eq!(
        flow.on_utterance("yes", Some(listening - Duration::from_millis(1)), listening),
        ApprovalVerdict::Ignored
    );
    assert_eq!(
        flow.on_utterance("hmm, let me think", Some(listening), listening),
        ApprovalVerdict::NotAnAnswer
    );
    assert!(flow.pending().is_some());
    assert_eq!(
        flow.on_utterance("Yes.", Some(listening), listening),
        ApprovalVerdict::Answered(7, ApprovalReply::Approve)
    );
    assert!(flow.pending().is_none());
    assert_eq!(
        flow.on_utterance("yes", Some(listening), listening),
        ApprovalVerdict::NotAnAnswer
    );
}

#[test]
fn spoken_answers_time_out() {
    let start = Instant::now();
    let mut flow = ApprovalFlow::default();
    flow.request(7, "ls", false);
    flow.prompt_played(start);
    let late = start + ECHO_TAIL + APPROVAL_LISTEN_FOR;
    assert!(!flow.check_timeout(late - Duration::from_millis(1)));
    assert!(flow.check_timeout(late));
    assert!(!flow.check_timeout(late), "reported once");
    assert_eq!(
        flow.on_utterance("yes", Some(late), late),
        ApprovalVerdict::Ignored
    );
}

#[test]
fn destructive_commands_need_the_screen() {
    let start = Instant::now();
    let mut flow = ApprovalFlow::default();
    flow.request(1, "rm -rf /", true);
    flow.prompt_played(start);
    let listening = start + ECHO_TAIL;
    assert_eq!(
        flow.on_utterance("yes", Some(listening), listening),
        ApprovalVerdict::Ignored
    );
    assert!(flow.pending().is_some());
    flow.resolved(&1);
    assert!(flow.pending().is_none());
}

#[test]
fn on_screen_answers_clear_the_pending_approval() {
    let start = Instant::now();
    let mut flow = ApprovalFlow::default();
    flow.request(1, "ls", false);
    flow.resolved(&2);
    assert!(flow.pending().is_some());
    flow.resolved(&1);
    assert_eq!(
        flow.on_utterance("no", Some(start), start),
        ApprovalVerdict::NotAnAnswer
    );
}
