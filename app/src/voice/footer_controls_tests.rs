use local_transcription::speech::playback::ConversationState;

use super::*;

#[test]
fn mic_click_follows_mode_when_hands_free_is_available() {
    assert_eq!(
        mic_click(DictationMode::ClickToRecord, true, false),
        MicClick::Record
    );
    assert_eq!(
        mic_click(DictationMode::HandsFree, true, false),
        MicClick::ToggleHandsFree
    );
}

#[test]
fn mic_click_records_when_hands_free_is_unavailable() {
    assert_eq!(
        mic_click(DictationMode::HandsFree, false, false),
        MicClick::Record
    );
}

#[test]
fn mic_click_stops_a_running_hands_free_session_in_any_mode() {
    assert_eq!(
        mic_click(DictationMode::ClickToRecord, true, true),
        MicClick::ToggleHandsFree
    );
    assert_eq!(
        mic_click(DictationMode::HandsFree, true, true),
        MicClick::ToggleHandsFree
    );
}

#[test]
fn mic_visual_is_live_while_hands_free_has_the_mic() {
    for state in [
        HandsFreeState::Listening,
        HandsFreeState::Hearing,
        HandsFreeState::Transcribing,
    ] {
        assert_eq!(
            mic_visual(DictationMode::ClickToRecord, true, state),
            MicVisual {
                live: true,
                tooltip: MicTooltip::StopHandsFree,
            }
        );
    }
}

#[test]
fn mic_visual_tooltip_follows_mode_when_idle() {
    assert_eq!(
        mic_visual(DictationMode::ClickToRecord, true, HandsFreeState::Off),
        MicVisual {
            live: false,
            tooltip: MicTooltip::Record,
        }
    );
    assert_eq!(
        mic_visual(DictationMode::HandsFree, true, HandsFreeState::Off),
        MicVisual {
            live: false,
            tooltip: MicTooltip::StartHandsFree,
        }
    );
    assert_eq!(
        mic_visual(DictationMode::HandsFree, false, HandsFreeState::Off).tooltip,
        MicTooltip::Record
    );
}

#[test]
fn conversation_visual_is_idle_when_off() {
    let visual = conversation_visual(false, ConversationState::Off, false);
    assert!(!visual.live);
    assert!(visual.label.is_empty());
    assert_eq!(visual.tooltip, "Start voice conversation");
}

#[test]
fn conversation_visual_shows_the_state_while_active() {
    for (state, label) in [
        (ConversationState::Off, "Starting"),
        (ConversationState::Listening, "Listening"),
        (ConversationState::Thinking, "Thinking"),
        (ConversationState::Speaking, "Speaking"),
    ] {
        let visual = conversation_visual(true, state, false);
        assert!(visual.live);
        assert_eq!(visual.label, label);
        assert_eq!(visual.tooltip, "Stop voice conversation");
    }
}

#[test]
fn conversation_visual_marks_a_muted_readout() {
    assert_eq!(
        conversation_visual(true, ConversationState::Speaking, true).label,
        "Speaking (muted)"
    );
}
