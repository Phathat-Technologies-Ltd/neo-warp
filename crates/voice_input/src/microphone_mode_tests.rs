use super::*;

fn state(active: MicMode, preferred: MicMode) -> Option<MicModeState> {
    Some(MicModeState { active, preferred })
}

#[test]
fn status_label_names_the_preferred_mode() {
    assert_eq!(
        status_label(
            state(MicMode::VoiceIsolation, MicMode::VoiceIsolation),
            false,
            true
        ),
        "Voice Isolation"
    );
    assert_eq!(
        status_label(
            state(MicMode::Standard, MicMode::WideSpectrum),
            false,
            false
        ),
        "Wide Spectrum"
    );
    assert_eq!(
        status_label(None, false, false),
        "Unavailable on this computer"
    );
}

#[test]
fn status_label_flags_a_mode_the_microphone_does_not_honour() {
    assert_eq!(
        status_label(
            state(MicMode::Standard, MicMode::VoiceIsolation),
            false,
            true
        ),
        "Voice Isolation (not active on this microphone)"
    );
}

#[test]
fn status_label_explains_system_wide_cancellation() {
    assert_eq!(
        status_label(
            state(MicMode::Standard, MicMode::VoiceIsolation),
            true,
            true
        ),
        "Not available while cancelling sound from other apps"
    );
}

#[test]
fn suggests_voice_isolation_only_on_voice_processing_when_not_chosen() {
    let standard = state(MicMode::Standard, MicMode::Standard);
    assert!(should_suggest_voice_isolation(
        standard,
        Some(AudioBackend::VoiceProcessing),
        false
    ));
    assert!(!should_suggest_voice_isolation(
        standard,
        Some(AudioBackend::VoiceProcessing),
        true
    ));
    assert!(!should_suggest_voice_isolation(
        standard,
        Some(AudioBackend::SystemAec),
        false
    ));
    assert!(!should_suggest_voice_isolation(
        standard,
        Some(AudioBackend::Cpal),
        false
    ));
    assert!(!should_suggest_voice_isolation(standard, None, false));
    assert!(!should_suggest_voice_isolation(
        state(MicMode::VoiceIsolation, MicMode::VoiceIsolation),
        Some(AudioBackend::VoiceProcessing),
        false
    ));
    assert!(!should_suggest_voice_isolation(
        None,
        Some(AudioBackend::VoiceProcessing),
        false
    ));
}
