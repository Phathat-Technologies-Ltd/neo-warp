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
    controller.on_message(ServerMessage::SpeechStart, false, now);
    assert_eq!(controller.state(), HandsFreeState::Hearing);
    controller.on_message(ServerMessage::SpeechEnd, false, now);
    assert_eq!(controller.state(), HandsFreeState::Transcribing);
    controller.on_message(final_(1, "hi"), false, now);
    assert_eq!(controller.state(), HandsFreeState::Listening);
    controller.stop();
    assert!(!controller.state().is_mic_on());
}

#[test]
fn partials_then_final_then_auto_send() {
    let now = Instant::now();
    let mut controller = started();
    controller.on_message(ServerMessage::SpeechStart, false, now);
    assert_eq!(
        controller.on_message(partial(1, "run the"), false, now),
        vec![HandsFreeEffect::ShowPartial("run the".to_owned())]
    );
    controller.on_message(ServerMessage::SpeechEnd, false, now);
    assert_eq!(
        controller.on_message(final_(1, "Run the tests."), false, now),
        vec![HandsFreeEffect::Commit("Run the tests.".to_owned())]
    );
    assert_eq!(controller.tick(now + Duration::from_millis(1000)), None);
    assert_eq!(
        controller.tick(now + Duration::from_millis(1200)),
        Some(HandsFreeEffect::Send)
    );
    assert_eq!(controller.tick(now + Duration::from_secs(5)), None);
}

#[test]
fn speech_restarts_the_auto_send_timer() {
    let now = Instant::now();
    let mut controller = started();
    controller.on_message(final_(1, "first"), false, now);
    controller.on_message(ServerMessage::SpeechStart, false, now);
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
    controller.on_message(final_(1, "first"), false, now);
    assert_eq!(controller.tick(now + Duration::from_secs(60)), None);
}

#[test]
fn voice_commands_send_cancel_and_insert_newlines() {
    let now = Instant::now();
    let mut controller = started();
    assert_eq!(
        controller.on_message(final_(1, "run the tests, send it."), false, now),
        vec![
            HandsFreeEffect::Commit("run the tests".to_owned()),
            HandsFreeEffect::Send
        ]
    );
    assert_eq!(
        controller.on_message(final_(2, "scratch that"), false, now),
        vec![HandsFreeEffect::ClearInput]
    );
    assert_eq!(
        controller.on_message(final_(3, "step one new line"), false, now),
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
        controller.on_message(final_(1, "send"), false, now),
        vec![HandsFreeEffect::Commit("send".to_owned())]
    );
}

#[test]
fn command_text_is_normalized_when_asked() {
    let now = Instant::now();
    let mut controller = started();
    assert_eq!(
        controller.on_message(final_(1, "cargo test dash dash no fail fast"), true, now),
        vec![HandsFreeEffect::Commit(
            "cargo test --no-fail-fast".to_owned()
        )]
    );
}

#[test]
fn typing_pauses_until_the_input_resets() {
    let now = Instant::now();
    let mut controller = started();
    controller.user_typed();
    assert!(controller.is_paused());
    assert!(
        controller
            .on_message(partial(1, "x"), false, now)
            .is_empty()
    );
    assert!(controller.on_message(final_(1, "x"), false, now).is_empty());
    assert_eq!(controller.tick(now + Duration::from_secs(5)), None);
    controller.input_reset();
    assert_eq!(
        controller.on_message(final_(2, "y"), false, now),
        vec![HandsFreeEffect::Commit("y".to_owned())]
    );
}

#[test]
fn an_empty_final_clears_a_shown_partial() {
    let now = Instant::now();
    let mut controller = started();
    controller.on_message(partial(1, "uh"), false, now);
    assert_eq!(
        controller.on_message(final_(1, ""), false, now),
        vec![HandsFreeEffect::Commit(String::new())]
    );
    assert!(controller.on_message(final_(2, ""), false, now).is_empty());
}

#[test]
fn messages_are_ignored_when_off() {
    let mut controller = HandsFreeController::default();
    assert!(
        controller
            .on_message(final_(1, "x"), false, Instant::now())
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
