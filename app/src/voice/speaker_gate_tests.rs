use super::*;

fn final_message(utterance: u64, text: &str) -> ServerMessage {
    ServerMessage::Final {
        utterance,
        text: text.to_owned(),
    }
}

fn pcm(len: usize, value: i16) -> Vec<u8> {
    std::iter::repeat_n(value.to_le_bytes(), len)
        .flatten()
        .collect()
}

/// Not enrolled (or the feature off, or no model) means `verify` is false: every message passes
/// straight through in order, exactly as without speaker verification.
#[test]
fn not_enrolled_sessions_pass_everything_through() {
    let mut gate = SpeakerGate::default();
    let messages = [
        ServerMessage::SpeechStart,
        ServerMessage::Partial {
            utterance: 1,
            text: "list".to_owned(),
        },
        ServerMessage::SpeechEnd,
        final_message(1, "list files"),
        final_message(2, "send"),
    ];
    for message in messages {
        assert_eq!(
            gate.push(message.clone(), 10_000, false),
            vec![GateStep::Deliver(message)]
        );
    }
    assert!(!gate.is_verifying());
}

#[test]
fn finals_pass_unchecked_when_verification_is_off() {
    let mut gate = SpeakerGate::default();
    let steps = gate.push(final_message(1, "list files"), 100, false);
    assert_eq!(
        steps,
        vec![GateStep::Deliver(final_message(1, "list files"))]
    );
}

#[test]
fn a_final_waits_for_its_check_with_the_utterance_range() {
    let mut gate = SpeakerGate::default();
    gate.push(ServerMessage::SpeechStart, 32_000, true);
    gate.push(ServerMessage::SpeechEnd, 64_000, true);
    let steps = gate.push(final_message(3, "list files"), 66_000, true);
    assert_eq!(
        steps,
        vec![GateStep::Verify(VerifyRequest {
            utterance: 3,
            text: "list files".to_owned(),
            range: 24_000..64_000,
        })]
    );
    assert!(gate.is_verifying());
}

#[test]
fn a_matching_final_is_delivered() {
    let mut gate = SpeakerGate::default();
    gate.push(final_message(1, "list files"), 50_000, true);
    let (steps, rejected) = gate.verdict(Verdict::Match, 50_000, true);
    assert!(!rejected);
    assert_eq!(
        steps,
        vec![GateStep::Deliver(final_message(1, "list files"))]
    );
}

#[test]
fn a_rejected_final_clears_its_partial() {
    let mut gate = SpeakerGate::default();
    gate.push(final_message(1, "list files"), 50_000, true);
    let (steps, rejected) = gate.verdict(Verdict::NoMatch, 50_000, true);
    assert!(rejected);
    assert_eq!(steps, vec![GateStep::Deliver(final_message(1, ""))]);
}

#[test]
fn an_unavailable_check_lets_the_final_through() {
    let mut gate = SpeakerGate::default();
    gate.push(final_message(1, "list files"), 50_000, true);
    let (steps, rejected) = gate.verdict(Verdict::Unavailable, 50_000, true);
    assert!(!rejected);
    assert_eq!(
        steps,
        vec![GateStep::Deliver(final_message(1, "list files"))]
    );
}

#[test]
fn stop_phrases_and_empty_finals_skip_the_check() {
    let mut gate = SpeakerGate::default();
    assert_eq!(
        gate.push(final_message(1, "Stop."), 50_000, true),
        vec![GateStep::Deliver(final_message(1, "Stop."))]
    );
    assert_eq!(
        gate.push(final_message(2, "  "), 50_000, true),
        vec![GateStep::Deliver(final_message(2, "  "))]
    );
    assert!(!gate.is_verifying());
}

#[test]
fn messages_behind_a_check_keep_their_order() {
    let mut gate = SpeakerGate::default();
    gate.push(final_message(1, "first"), 50_000, true);
    assert!(
        gate.push(ServerMessage::SpeechStart, 51_000, true)
            .is_empty()
    );
    let partial = ServerMessage::Partial {
        utterance: 2,
        text: "sec".to_owned(),
    };
    assert!(gate.push(partial.clone(), 52_000, true).is_empty());
    assert!(
        gate.push(final_message(2, "second"), 53_000, true)
            .is_empty()
    );

    let (steps, _) = gate.verdict(Verdict::NoMatch, 60_000, true);
    // The partial is held for its own early check, which the final overtakes.
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[0], GateStep::Deliver(final_message(1, "")));
    assert_eq!(steps[1], GateStep::Deliver(ServerMessage::SpeechStart));
    assert!(matches!(&steps[2], GateStep::Verify(request) if request.text == "second"));

    let (steps, _) = gate.verdict(Verdict::Match, 60_000, true);
    assert_eq!(steps, vec![GateStep::Deliver(final_message(2, "second"))]);
}

#[test]
fn tap_slices_by_absolute_index_and_drops_old_audio() {
    let mut tap = AudioTap::default();
    tap.push_pcm(&pcm(100, 16_384));
    tap.push_pcm(&pcm(100, -16_384));
    assert_eq!(tap.total(), 200);
    let slice = tap.slice(90..110);
    assert_eq!(slice.len(), 20);
    assert_eq!(slice[0], 0.5);
    assert_eq!(slice[19], -0.5);
    assert_eq!(tap.slice(150..1_000).len(), 50);

    tap.push_pcm(&pcm(TAP_SAMPLES, 0));
    assert!(tap.slice(0..200).is_empty());
    assert_eq!(tap.slice(0..tap.total()).len(), TAP_SAMPLES);
}

fn partial(utterance: u64, text: &str) -> ServerMessage {
    ServerMessage::Partial {
        utterance,
        text: text.to_owned(),
    }
}

/// Starts utterance 1 and pushes partials until its early check starts.
fn start_early_check(gate: &mut SpeakerGate) {
    gate.push(ServerMessage::SpeechStart, 16_000, true);
    assert!(gate.push(partial(1, "li"), 20_000, true).is_empty());
    let steps = gate.push(partial(1, "list"), 30_000, true);
    assert_eq!(
        steps,
        vec![GateStep::EarlyVerify(VerifyRequest {
            utterance: 1,
            text: String::new(),
            range: 8_000..30_000,
        })]
    );
}

#[test]
fn partials_are_held_until_the_early_verdict() {
    let mut gate = SpeakerGate::default();
    start_early_check(&mut gate);
    assert!(gate.push(partial(1, "list fi"), 32_000, true).is_empty());
}

#[test]
fn an_early_match_releases_partials_and_still_checks_the_final() {
    let mut gate = SpeakerGate::default();
    start_early_check(&mut gate);
    gate.push(partial(1, "list fi"), 32_000, true);
    let (steps, rejected) = gate.early_verdict(1, Verdict::Match);
    assert!(!rejected);
    assert_eq!(steps, vec![GateStep::Deliver(partial(1, "list fi"))]);
    assert_eq!(
        gate.push(partial(1, "list files"), 34_000, true),
        vec![GateStep::Deliver(partial(1, "list files"))]
    );
    let steps = gate.push(final_message(1, "list files"), 40_000, true);
    assert!(matches!(&steps[..], [GateStep::Verify(request)] if request.utterance == 1));
}

#[test]
fn an_early_mismatch_suppresses_the_utterance_and_clears_it() {
    let mut gate = SpeakerGate::default();
    start_early_check(&mut gate);
    let (steps, rejected) = gate.early_verdict(1, Verdict::NoMatch);
    assert!(rejected);
    assert_eq!(steps, vec![GateStep::Deliver(final_message(1, ""))]);
    assert!(gate.push(partial(1, "list files"), 34_000, true).is_empty());
    assert_eq!(gate.push(ServerMessage::SpeechEnd, 36_000, true).len(), 1);
    assert!(
        gate.push(final_message(1, "list files"), 40_000, true)
            .is_empty()
    );
    assert!(!gate.is_verifying());

    gate.push(ServerMessage::SpeechStart, 50_000, true);
    assert!(gate.push(partial(2, "hi"), 52_000, true).is_empty());
}

#[test]
fn stop_phrases_pass_after_an_early_mismatch() {
    let mut gate = SpeakerGate::default();
    start_early_check(&mut gate);
    gate.early_verdict(1, Verdict::NoMatch);
    assert_eq!(
        gate.push(final_message(1, "Stop."), 40_000, true),
        vec![GateStep::Deliver(final_message(1, "Stop."))]
    );
}

#[test]
fn a_stale_early_verdict_is_ignored() {
    let mut gate = SpeakerGate::default();
    start_early_check(&mut gate);
    gate.push(final_message(1, "list files"), 40_000, true);
    assert_eq!(gate.early_verdict(1, Verdict::NoMatch), (Vec::new(), false));
}
