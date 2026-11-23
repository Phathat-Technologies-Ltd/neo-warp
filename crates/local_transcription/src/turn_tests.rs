use super::*;

fn end_of_turn() -> EndOfTurn {
    EndOfTurn::new(EndOfTurnConfig {
        probe_silence_ms: 200,
        threshold: 0.5,
        incomplete_silence_ms: 2000,
    })
}

#[test]
fn probes_once_per_pause() {
    let mut turn = end_of_turn();
    assert_eq!(turn.on_silence(None), TurnDecision::Wait);
    assert_eq!(turn.on_silence(Some(100)), TurnDecision::Wait);
    assert_eq!(turn.on_silence(Some(200)), TurnDecision::Probe);
    assert_eq!(turn.on_silence(Some(500)), TurnDecision::Wait);
    assert_eq!(turn.on_silence(Some(2000)), TurnDecision::End);
}

#[test]
fn speech_rearms_the_probe() {
    let mut turn = end_of_turn();
    assert_eq!(turn.on_silence(Some(224)), TurnDecision::Probe);
    assert_eq!(turn.on_silence(None), TurnDecision::Wait);
    assert_eq!(turn.on_silence(Some(224)), TurnDecision::Probe);
}

#[test]
fn threshold_decides_completion() {
    let turn = end_of_turn();
    assert!(turn.is_complete(0.5));
    assert!(!turn.is_complete(0.49));
}

#[test]
fn trailing_function_words_mean_mid_thought() {
    assert!(ends_mid_thought("list the files in"));
    assert!(ends_mid_thought("Stop the server, um,"));
    assert!(ends_mid_thought("check whether the"));
    assert!(!ends_mid_thought("list the files in src"));
    assert!(!ends_mid_thought("Is it in?"));
    assert!(!ends_mid_thought(""));
}
