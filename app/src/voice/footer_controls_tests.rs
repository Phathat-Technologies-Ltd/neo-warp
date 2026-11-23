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
fn mic_tooltip_follows_mode_when_idle() {
    assert_eq!(
        mic_tooltip(DictationMode::ClickToRecord, true, false),
        MicTooltip::Record
    );
    assert_eq!(
        mic_tooltip(DictationMode::HandsFree, true, false),
        MicTooltip::StartHandsFree
    );
    assert_eq!(
        mic_tooltip(DictationMode::HandsFree, false, false),
        MicTooltip::Record
    );
}

#[test]
fn mic_tooltip_offers_to_stop_a_running_hands_free_session() {
    for mode in [DictationMode::ClickToRecord, DictationMode::HandsFree] {
        assert_eq!(mic_tooltip(mode, true, true), MicTooltip::StopHandsFree);
    }
}
