use super::*;

fn partial(utterance: u64, text: &str) -> ServerMessage {
    ServerMessage::Partial {
        utterance,
        text: text.to_owned(),
    }
}

fn final_(utterance: u64, text: &str) -> ServerMessage {
    ServerMessage::Final {
        utterance,
        text: text.to_owned(),
    }
}

fn started() -> HandsFreeController {
    let mut controller = HandsFreeController::default();
    controller.start(HandsFreeOptions::default());
    controller
}

#[test]
fn indicator_follows_speech() {
    let now = Instant::now();
    let mut controller = started();
    assert_eq!(controller.state(), HandsFreeState::Listening);
    controller.on_message(ServerMessage::SpeechStart, now);
    assert_eq!(controller.state(), HandsFreeState::Hearing);
    controller.on_message(ServerMessage::SpeechEnd, now);
    assert_eq!(controller.state(), HandsFreeState::Transcribing);
    controller.on_message(final_(1, "hi"), now);
    assert_eq!(controller.state(), HandsFreeState::Listening);
    controller.stop();
    assert!(!controller.state().is_mic_on());
}

#[test]
fn partials_then_final_then_auto_send() {
    let now = Instant::now();
    let mut controller = started();
    controller.on_message(ServerMessage::SpeechStart, now);
    assert_eq!(
        controller.on_message(partial(1, "run the"), now),
        vec![HandsFreeEffect::ShowPartial("run the".to_owned())]
    );
    controller.on_message(ServerMessage::SpeechEnd, now);
    assert_eq!(
        controller.on_message(final_(1, "Run the tests."), now),
        vec![HandsFreeEffect::Commit("Run the tests.".to_owned())]
    );
    assert_eq!(controller.tick(now + Duration::from_millis(1000)), None);
    assert_eq!(
        controller.tick(now + Duration::from_millis(1200)),
        Some(HandsFreeEffect::AutoSend)
    );
    assert_eq!(controller.tick(now + Duration::from_secs(5)), None);
}

#[test]
fn speech_restarts_the_auto_send_timer() {
    let now = Instant::now();
    let mut controller = started();
    controller.on_message(final_(1, "first"), now);
    controller.on_message(ServerMessage::SpeechStart, now);
    assert_eq!(controller.tick(now + Duration::from_secs(2)), None);
}

#[test]
fn auto_send_can_be_off() {
    let now = Instant::now();
    let mut controller = HandsFreeController::default();
    controller.start(HandsFreeOptions {
        auto_send_after: None,
        voice_commands: true,
    });
    controller.on_message(final_(1, "first"), now);
    assert_eq!(controller.tick(now + Duration::from_secs(60)), None);
}

#[test]
fn voice_commands_send_cancel_and_insert_newlines() {
    let now = Instant::now();
    let mut controller = started();
    assert_eq!(
        controller.on_message(final_(1, "run the tests, send it."), now),
        vec![
            HandsFreeEffect::Commit("run the tests".to_owned()),
            HandsFreeEffect::Send
        ]
    );
    assert_eq!(
        controller.on_message(final_(2, "scratch that"), now),
        vec![HandsFreeEffect::ClearInput]
    );
    assert_eq!(
        controller.on_message(final_(3, "step one new line"), now),
        vec![
            HandsFreeEffect::Commit("step one".to_owned()),
            HandsFreeEffect::InsertNewline
        ]
    );
    assert_eq!(controller.auto_send_at(), None);
}

#[test]
fn voice_commands_can_be_off() {
    let now = Instant::now();
    let mut controller = HandsFreeController::default();
    controller.start(HandsFreeOptions {
        auto_send_after: None,
        voice_commands: false,
    });
    assert_eq!(
        controller.on_message(final_(1, "send"), now),
        vec![HandsFreeEffect::Commit("send".to_owned())]
    );
}

#[test]
fn finals_are_normalized_only_for_shell_inputs() {
    assert_eq!(
        final_text_for_input("cargo test dash dash no fail fast", true),
        "cargo test --no-fail-fast"
    );
    assert_eq!(
        final_text_for_input("Explain this error, please.", false),
        "Explain this error, please."
    );
}

#[test]
fn auto_send_runs_only_low_risk_shell_commands() {
    assert!(sends_input(
        &HandsFreeEffect::AutoSend,
        false,
        "rm -rf build"
    ));
    assert!(sends_input(&HandsFreeEffect::AutoSend, true, "git status"));
    assert!(!sends_input(
        &HandsFreeEffect::AutoSend,
        true,
        "rm -rf build"
    ));
    assert!(sends_input(&HandsFreeEffect::Send, true, "rm -rf build"));
    assert!(!sends_input(&HandsFreeEffect::ClearInput, false, "ls"));
}

#[test]
fn typing_pauses_until_the_input_resets() {
    let now = Instant::now();
    let mut controller = started();
    controller.user_typed();
    assert!(controller.is_paused());
    assert!(controller.on_message(partial(1, "x"), now).is_empty());
    assert!(controller.on_message(final_(1, "x"), now).is_empty());
    assert_eq!(controller.tick(now + Duration::from_secs(5)), None);
    controller.input_reset();
    assert_eq!(
        controller.on_message(final_(2, "y"), now),
        vec![HandsFreeEffect::Commit("y".to_owned())]
    );
}

#[test]
fn an_empty_final_clears_a_shown_partial() {
    let now = Instant::now();
    let mut controller = started();
    controller.on_message(partial(1, "uh"), now);
    assert_eq!(
        controller.on_message(final_(1, ""), now),
        vec![HandsFreeEffect::Commit(String::new())]
    );
    assert!(controller.on_message(final_(2, ""), now).is_empty());
}

#[test]
fn messages_are_ignored_when_off() {
    let mut controller = HandsFreeController::default();
    assert!(
        controller
            .on_message(final_(1, "x"), Instant::now())
            .is_empty()
    );
}

#[test]
fn pending_range_replaces_partials_in_place() {
    let mut range = PendingVoiceRange::default();
    let buffer = range.replace("git", "che");
    assert_eq!(buffer, "git che");
    let buffer = range.replace(&buffer, "checkout");
    assert_eq!(buffer, "git checkout");
    let buffer = range.commit(&buffer, "checkout main");
    assert_eq!(buffer, "git checkout main");
    // The next utterance opens a new range after the committed text.
    let buffer = range.replace(&buffer, "dash");
    assert_eq!(buffer, "git checkout main dash");
}

#[test]
fn pending_range_starts_without_a_space_in_an_empty_input() {
    let mut range = PendingVoiceRange::default();
    assert_eq!(range.commit("", "ls"), "ls");
    assert_eq!(range.commit("echo ", "hi"), "echo hi");
}

#[test]
fn pending_range_drops_an_empty_utterance() {
    let mut range = PendingVoiceRange::default();
    let buffer = range.replace("ls", "uh");
    assert_eq!(range.commit(&buffer, ""), "ls");
    assert_eq!(range.commit("ls", ""), "ls");
}

#[test]
fn pending_range_recovers_when_the_buffer_shrank() {
    let mut range = PendingVoiceRange::default();
    range.replace("a long prefix", "partial");
    assert_eq!(range.replace("a", "x"), "a x");
}

#[test]
fn natural_language_routes_to_agent() {
    for text in [
        "Hey, can you please list all the items?",
        "list all the items",
        "make sure the tests pass",
        "find me the config file",
        "git is confusing, what does rebase do?",
        "run git status and tell me what changed",
        "Run, list all the items",
    ] {
        assert_eq!(
            route_utterance(text),
            (InputRoute::Agent, text.to_owned()),
            "{text}"
        );
    }
}

#[test]
fn clear_commands_route_to_shell() {
    for text in ["git status", "ls", "cargo test -p warp", "make build"] {
        assert_eq!(
            route_utterance(text),
            (InputRoute::Shell, text.to_owned()),
            "{text}"
        );
    }
}

#[test]
fn spoken_run_prefix_routes_to_shell_without_the_prefix() {
    assert_eq!(
        route_utterance("Run, git status"),
        (InputRoute::Shell, "git status".to_owned())
    );
    assert_eq!(
        route_utterance("command echo hello"),
        (InputRoute::Shell, "echo hello".to_owned())
    );
    assert_eq!(
        route_utterance("run"),
        (InputRoute::Agent, "run".to_owned())
    );
}

#[test]
fn low_risk_commands_are_read_only() {
    for command in [
        "ls -la",
        "pwd",
        "cat README.md",
        "git status",
        "git log -5",
        "git diff",
    ] {
        assert!(is_low_risk_command(command), "{command}");
    }
    for command in [
        "rm -rf build",
        "mv a b",
        "sudo ls",
        "git push --force",
        "git checkout .",
        "git",
        "cat a > b",
        "ls; rm x",
        "ls $(rm x)",
        "git diff --output=patch",
        "make build",
        "",
    ] {
        assert!(!is_low_risk_command(command), "{command}");
    }
}

#[test]
fn starts_input_ignores_the_open_utterance() {
    let mut range = PendingVoiceRange::default();
    assert!(range.starts_input(""));
    let buffer = range.replace("", "hello");
    assert!(range.starts_input(&buffer));
    let mut range = PendingVoiceRange::default();
    let buffer = range.replace("git", "status");
    assert!(!range.starts_input(&buffer));
}

#[test]
fn deferred_auto_send_waits_for_the_new_time() {
    let now = Instant::now();
    let mut controller = started();
    controller.on_message(final_(1, "run the tests"), now);
    controller.defer_auto_send(now + Duration::from_secs(3));
    assert_eq!(controller.tick(now + Duration::from_secs(2)), None);
    assert_eq!(
        controller.tick(now + Duration::from_secs(3)),
        Some(HandsFreeEffect::AutoSend)
    );
}
