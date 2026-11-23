use super::*;

#[test]
fn common_prefix_respects_char_boundaries() {
    assert_eq!(common_prefix_len("hello", "hello world"), 5);
    assert_eq!(common_prefix_len("hello world", "hello"), 5);
    assert_eq!(common_prefix_len("abc", "abd"), 2);
    assert_eq!(common_prefix_len("café ok", "café no"), 6);
    assert_eq!(common_prefix_len("é", "è"), 0);
    assert_eq!(common_prefix_len("", "x"), 0);
}

#[test]
fn replies_follow_the_hands_free_target() {
    let (left, right) = (EntityId::from_usize(1), EntityId::from_usize(2));
    assert!(surface_changes(None, Some(left)));
    assert!(surface_changes(Some(left), Some(right)));
    assert!(!surface_changes(Some(left), Some(left)));
    assert!(!surface_changes(Some(left), None));
}

#[test]
fn pocket_speaks_with_its_only_voice() {
    let voices = new_speaker(SpeechOutputEngine::Pocket).voices();
    assert_eq!(
        choose_voice(&voices, "af_heart").map(|voice| voice.as_str().to_owned()),
        Some("alba".to_owned())
    );
    let kokoro = new_speaker(SpeechOutputEngine::Kokoro).voices();
    assert_eq!(
        choose_voice(&kokoro, "").map(|voice| voice.as_str().to_owned()),
        kokoro.first().map(|voice| voice.id.as_str().to_owned())
    );
}

#[test]
fn idle_timeout_zero_means_never() {
    assert_eq!(idle_timeout(0), None);
    assert_eq!(idle_timeout(5), Some(Duration::from_secs(300)));
}

#[test]
fn idle_check_expires_only_after_the_timeout_since_the_last_activity() {
    let start = Instant::now();
    let timeout = Duration::from_secs(300);
    assert_eq!(
        check_idle(start, timeout, start + Duration::from_secs(10)),
        IdleCheck::CheckAt(start + timeout)
    );
    assert_eq!(
        check_idle(start, timeout, start + timeout),
        IdleCheck::Expired
    );
    // Activity resets the clock.
    let later = start + Duration::from_secs(200);
    assert_eq!(
        check_idle(later, timeout, start + timeout),
        IdleCheck::CheckAt(later + timeout)
    );
}

#[test]
fn idle_ended_message_names_the_timeout() {
    assert_eq!(
        idle_ended_message(5),
        "Voice conversation ended after 5 minutes without speech"
    );
    assert_eq!(
        idle_ended_message(1),
        "Voice conversation ended after 1 minute without speech"
    );
}

fn voice_info(id: &str) -> VoiceInfo {
    VoiceInfo {
        id: VoiceId::new(id),
        display_name: id.to_owned(),
    }
}

fn wanted(engine: SpeechOutputEngine, voice: &str, rate: f32) -> WantedSpeech {
    WantedSpeech {
        engine,
        voice: voice.to_owned(),
        rate,
    }
}

#[test]
fn a_new_engine_replaces_the_speaker() {
    let voices = [voice_info("af_heart")];
    assert_eq!(
        speech_setting_change(
            SpeechOutputEngine::Kokoro,
            SpeechOutputEngine::Kokoro,
            &VoiceId::new("af_heart"),
            1.0,
            &wanted(SpeechOutputEngine::Pocket, "", 1.0),
            &voices,
        ),
        SpeechSettingChange::Engine(SpeechOutputEngine::Pocket)
    );
}

#[test]
fn a_new_voice_or_rate_keeps_the_speaker() {
    let voices = [voice_info("af_heart"), voice_info("am_adam")];
    let current = VoiceId::new("af_heart");
    let kokoro = SpeechOutputEngine::Kokoro;
    assert_eq!(
        speech_setting_change(
            kokoro,
            kokoro,
            &current,
            1.0,
            &wanted(kokoro, "am_adam", 1.0),
            &voices
        ),
        SpeechSettingChange::Voice {
            voice: VoiceId::new("am_adam"),
            rate: 1.0
        }
    );
    assert_eq!(
        speech_setting_change(
            kokoro,
            kokoro,
            &current,
            1.0,
            &wanted(kokoro, "af_heart", 1.2),
            &voices
        ),
        SpeechSettingChange::Voice {
            voice: current.clone(),
            rate: 1.2
        }
    );
    assert_eq!(
        speech_setting_change(
            kokoro,
            kokoro,
            &current,
            1.0,
            &wanted(kokoro, "af_heart", 1.0),
            &voices
        ),
        SpeechSettingChange::Unchanged
    );
}

#[test]
fn an_unknown_voice_falls_back_to_the_engine_default() {
    let voices = [voice_info("af_heart"), voice_info("am_adam")];
    let kokoro = SpeechOutputEngine::Kokoro;
    assert_eq!(
        speech_setting_change(
            kokoro,
            kokoro,
            &VoiceId::new("am_adam"),
            1.0,
            &wanted(kokoro, "", 1.0),
            &voices
        ),
        SpeechSettingChange::Voice {
            voice: VoiceId::new("af_heart"),
            rate: 1.0
        }
    );
}

#[test]
fn voice_changes_wait_while_a_fallback_engine_speaks() {
    let voices = [voice_info("af_heart")];
    assert_eq!(
        speech_setting_change(
            SpeechOutputEngine::TinyVoca,
            SpeechOutputEngine::Kokoro,
            &VoiceId::new("af_heart"),
            1.0,
            &wanted(SpeechOutputEngine::TinyVoca, "other", 1.5),
            &voices,
        ),
        SpeechSettingChange::Unchanged
    );
}

#[test]
fn only_speech_settings_trigger_a_switch() {
    assert!(is_speech_setting_event(
        &AISettingsChangedEvent::ConversationVoice {
            change_event_reason: warp_core::settings::ChangeEventReason::LocalChange,
        }
    ));
}
