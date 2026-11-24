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
fn prompt_speaks_the_subject_verbatim() {
    assert_eq!(
        approval_prompt("run a cargo command", false),
        "I want to run a cargo command. Say yes, or no."
    );
    assert_eq!(
        approval_prompt("run a command that deletes files", true),
        "I want to run a command that deletes files. Approve on screen."
    );
}

#[test]
fn common_affirmatives_and_negatives_are_answers() {
    for text in [
        "Run it.",
        "yes",
        "Go ahead.",
        "Do it.",
        "Okay, run.",
        "Okay run it",
        "Sure.",
        "Yeah.",
        "Run.",
    ] {
        assert_eq!(match_approval(text), Some(ApprovalReply::Approve), "{text}");
    }
    for text in ["No.", "Don't.", "Cancel.", "Stop.", "Don't run it.", "Nah."] {
        assert_eq!(match_approval(text), Some(ApprovalReply::Reject), "{text}");
    }
}

#[test]
fn vague_words_are_not_answers() {
    for text in ["ok", "okay", "maybe", "it", "run the tests"] {
        assert_eq!(match_approval(text), None, "{text}");
    }
}

/// A flow whose prompt for approval 7 has played, and the time answers are accepted from.
fn listening_flow() -> (ApprovalFlow<u32>, Instant) {
    let start = Instant::now();
    let mut flow = ApprovalFlow::default();
    flow.request(7, "ls", false);
    flow.prompt_played(start);
    (flow, start + ECHO_TAIL)
}

#[test]
fn an_answer_missing_its_first_word_is_asked_again_never_sent() {
    let (mut flow, listening) = listening_flow();
    assert_eq!(
        flow.on_utterance("It.", Some(listening), listening),
        ApprovalVerdict::Unclear
    );
    assert!(flow.pending().is_some());
    assert_eq!(
        flow.on_utterance("Run it.", Some(listening), listening),
        ApprovalVerdict::Answered(7, ApprovalReply::Approve)
    );
}

#[test]
fn the_rest_of_a_split_answer_is_dropped() {
    let (mut flow, listening) = listening_flow();
    assert_eq!(
        flow.on_utterance("Run", Some(listening), listening),
        ApprovalVerdict::Answered(7, ApprovalReply::Approve)
    );
    let later = listening + Duration::from_millis(600);
    assert_eq!(
        flow.on_utterance("it.", Some(later), later),
        ApprovalVerdict::Ignored
    );
    // Only once, and only within the join window.
    assert_eq!(
        flow.on_utterance("it.", Some(later), later),
        ApprovalVerdict::NotAnAnswer
    );
}

#[test]
fn a_split_answer_is_joined() {
    let (mut flow, listening) = listening_flow();
    assert_eq!(
        flow.on_utterance("Do", Some(listening), listening),
        ApprovalVerdict::Held
    );
    let later = listening + Duration::from_secs(1);
    assert_eq!(
        flow.on_utterance("it.", Some(later), later),
        ApprovalVerdict::Answered(7, ApprovalReply::Approve)
    );

    let (mut flow, listening) = listening_flow();
    assert_eq!(
        flow.on_utterance("Don't.", Some(listening), listening),
        ApprovalVerdict::Answered(7, ApprovalReply::Reject)
    );
    let later = listening + Duration::from_secs(1);
    assert_eq!(
        flow.on_utterance("run it", Some(later), later),
        ApprovalVerdict::Ignored
    );
}

#[test]
fn a_held_start_expires_after_the_join_window() {
    let (mut flow, listening) = listening_flow();
    assert_eq!(
        flow.on_utterance("okay", Some(listening), listening),
        ApprovalVerdict::Held
    );
    let late = listening + ANSWER_JOIN_WINDOW + Duration::from_millis(1);
    assert_eq!(
        flow.on_utterance("it", Some(late), late),
        ApprovalVerdict::Unclear
    );
}

#[test]
fn an_interrupted_prompt_accepts_the_speech_that_cut_it_off() {
    let start = Instant::now();
    let mut flow = ApprovalFlow::default();
    flow.request(7, "ls", false);
    let talking_from = start + Duration::from_millis(300);
    flow.prompt_interrupted(talking_from, start + Duration::from_millis(600));
    let now = start + Duration::from_secs(1);
    assert!(flow.accepts_answer(Some(talking_from), now));
    assert_eq!(
        flow.on_utterance("run it", Some(talking_from), now),
        ApprovalVerdict::Answered(7, ApprovalReply::Approve)
    );
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
