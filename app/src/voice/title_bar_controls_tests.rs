use local_transcription::speech::playback::ConversationState;

use super::*;

#[test]
fn hands_free_status_is_live_only_while_the_mic_is_open() {
    for state in [
        HandsFreeState::Listening,
        HandsFreeState::Hearing,
        HandsFreeState::Transcribing,
    ] {
        let status = hands_free_status(state);
        assert!(status.live);
        assert!(status.tooltip.contains(state.label()), "{}", status.tooltip);
    }
    assert!(!hands_free_status(HandsFreeState::Paused).live);
    assert_eq!(
        hands_free_status(HandsFreeState::Off),
        ButtonStatus {
            live: false,
            tooltip: "Start hands-free voice input".to_owned(),
        }
    );
}

#[test]
fn conversation_status_is_idle_when_off() {
    assert_eq!(
        conversation_status(false, ConversationState::Off, false),
        ButtonStatus {
            live: false,
            tooltip: "Start voice conversation".to_owned(),
        }
    );
}

#[test]
fn conversation_status_names_the_state_in_the_tooltip() {
    for (state, label) in [
        (ConversationState::Off, "Starting"),
        (ConversationState::Listening, "Listening"),
        (ConversationState::Thinking, "Thinking"),
        (ConversationState::Speaking, "Speaking"),
    ] {
        let status = conversation_status(true, state, false);
        assert!(status.live);
        assert_eq!(
            status.tooltip,
            format!("Voice conversation: {label} \u{b7} Stop")
        );
    }
    assert_eq!(
        conversation_status(true, ConversationState::Speaking, true).tooltip,
        "Voice conversation: Speaking, muted \u{b7} Stop"
    );
}

#[test]
fn waveform_is_flat_when_silent_or_not_live() {
    let flat = [BAR_MIN_HEIGHT; 4];
    assert_eq!(waveform_bar_heights(0., true), flat);
    assert_eq!(waveform_bar_heights(1., false), flat);
}

#[test]
fn waveform_bars_grow_with_the_level() {
    let quiet = waveform_bar_heights(0.2, true);
    let loud = waveform_bar_heights(0.9, true);
    for (quiet, loud) in quiet.iter().zip(loud) {
        assert!(loud > *quiet);
        assert!(loud <= BAR_MAX_HEIGHT);
    }
    assert_eq!(waveform_bar_heights(5., true)[1], BAR_MAX_HEIGHT);
}
