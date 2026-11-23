use super::{
    AgentAttributionToggleState, GrokSubscriptionButtonAction, SettingsWidget,
    WarpCloudModelsEnabledWidget, derive_agent_attribution_toggle_state,
    grok_subscription_button_action,
};
use crate::workspaces::workspace::AdminEnablementSetting;

#[test]
fn respect_user_setting_returns_user_pref_unlocked() {
    let state = derive_agent_attribution_toggle_state(
        &AdminEnablementSetting::RespectUserSetting,
        true,
        true,
    );
    assert_eq!(
        state,
        AgentAttributionToggleState {
            is_enabled: true,
            is_forced_by_org: false,
            is_disabled: false,
        }
    );
}

#[test]
fn respect_user_setting_with_user_off_returns_unchecked_unlocked() {
    let state = derive_agent_attribution_toggle_state(
        &AdminEnablementSetting::RespectUserSetting,
        false,
        true,
    );
    assert_eq!(
        state,
        AgentAttributionToggleState {
            is_enabled: false,
            is_forced_by_org: false,
            is_disabled: false,
        }
    );
}

#[test]
fn team_enable_locks_toggle_on_regardless_of_user_pref() {
    let state = derive_agent_attribution_toggle_state(&AdminEnablementSetting::Enable, false, true);
    assert_eq!(
        state,
        AgentAttributionToggleState {
            is_enabled: true,
            is_forced_by_org: true,
            is_disabled: true,
        }
    );
}

#[test]
fn team_disable_locks_toggle_off_regardless_of_user_pref() {
    let state = derive_agent_attribution_toggle_state(&AdminEnablementSetting::Disable, true, true);
    assert_eq!(
        state,
        AgentAttributionToggleState {
            is_enabled: false,
            is_forced_by_org: true,
            is_disabled: true,
        }
    );
}

#[test]
fn ai_globally_disabled_marks_toggle_disabled_but_not_forced() {
    let state = derive_agent_attribution_toggle_state(
        &AdminEnablementSetting::RespectUserSetting,
        true,
        false,
    );
    assert_eq!(
        state,
        AgentAttributionToggleState {
            is_enabled: true,
            is_forced_by_org: false,
            is_disabled: true,
        }
    );
}

#[test]
fn team_force_takes_precedence_over_global_ai_disabled() {
    let state =
        derive_agent_attribution_toggle_state(&AdminEnablementSetting::Enable, false, false);
    assert_eq!(
        state,
        AgentAttributionToggleState {
            is_enabled: true,
            is_forced_by_org: true,
            is_disabled: true,
        }
    );
}

#[test]
fn grok_button_action_reflects_tokens_and_attempt_phase() {
    assert_eq!(
        grok_subscription_button_action(false, None),
        GrokSubscriptionButtonAction::Connect
    );
    assert_eq!(
        grok_subscription_button_action(false, Some(false)),
        GrokSubscriptionButtonAction::Cancel
    );
    assert_eq!(
        grok_subscription_button_action(false, Some(true)),
        GrokSubscriptionButtonAction::Cancelling
    );
    // Stored tokens take precedence regardless of attempt phase.
    assert_eq!(
        grok_subscription_button_action(true, None),
        GrokSubscriptionButtonAction::Disconnect
    );
    assert_eq!(
        grok_subscription_button_action(true, Some(false)),
        GrokSubscriptionButtonAction::Disconnect
    );
    assert_eq!(
        grok_subscription_button_action(true, Some(true)),
        GrokSubscriptionButtonAction::Disconnect
    );
}

#[test]
fn warp_cloud_models_widget_is_found_by_allowlist_searches() {
    let terms = WarpCloudModelsEnabledWidget::default()
        .search_terms()
        .to_owned();
    for query in ["warp cloud", "allowlist", "byo", "custom inference"] {
        assert!(terms.contains(query), "{query}");
    }
}

#[cfg(not(target_family = "wasm"))]
mod voice_pickers {
    use super::super::{
        PickerOptions, SettingsWidget, VoiceWidget, WarpAgentPageAction,
        conversation_voice_options, idle_timeout_options, speech_engine_options,
        speech_rate_options,
    };
    use crate::voice::local_model::SpeechOutputEngine;

    fn labels(options: &PickerOptions) -> Vec<&str> {
        options
            .items
            .iter()
            .map(|(label, _)| label.as_str())
            .collect()
    }

    #[test]
    fn voice_picker_lists_every_voice_and_selects_the_current_one() {
        let voices = vec![
            ("premium".to_owned(), "Ava (Premium, en-US)".to_owned()),
            ("boing".to_owned(), "Boing (Default, en-US)".to_owned()),
            (
                "personal-voice-request".to_owned(),
                "Personal Voice (asks for permission)".to_owned(),
            ),
        ];
        let options = conversation_voice_options(voices.clone(), "boing");
        assert_eq!(
            labels(&options),
            [
                "Ava (Premium, en-US)",
                "Boing (Default, en-US)",
                "Personal Voice (asks for permission)"
            ]
        );
        assert_eq!(options.selected, 1);
        assert_eq!(
            options.items[2].1,
            WarpAgentPageAction::SetConversationVoice("personal-voice-request".to_owned())
        );
        assert_eq!(conversation_voice_options(voices, "").selected, 0);
    }

    #[test]
    fn engine_picker_selects_the_current_engine() {
        let options = speech_engine_options("pocket");
        assert_eq!(options.items.len(), SpeechOutputEngine::all().len());
        assert_eq!(
            options.items[options.selected].1,
            WarpAgentPageAction::SetConversationSpeechEngine("pocket")
        );
        assert_eq!(speech_engine_options("unknown").selected, 0);
    }

    #[test]
    fn rate_picker_keeps_an_unlisted_current_rate() {
        let options = speech_rate_options(100);
        assert_eq!(labels(&options), ["0.8x", "1.0x", "1.2x", "1.35x", "1.5x"]);
        assert_eq!(options.selected, 1);

        let options = speech_rate_options(110);
        assert_eq!(options.items.len(), 6);
        assert_eq!(
            options.items[options.selected].1,
            WarpAgentPageAction::SetConversationSpeechRate(110)
        );
    }

    #[test]
    fn idle_timeout_picker_lists_never_last() {
        let options = idle_timeout_options(5);
        assert_eq!(
            labels(&options),
            [
                "2 minutes",
                "5 minutes",
                "10 minutes",
                "15 minutes",
                "30 minutes",
                "Never"
            ]
        );
        assert_eq!(options.selected, 1);

        let options = idle_timeout_options(45);
        assert_eq!(options.items.last().unwrap().0, "Never");
        assert_eq!(options.items[options.selected].0, "45 minutes");
    }

    #[test]
    fn voice_widget_is_found_by_picker_searches() {
        let terms = VoiceWidget::default().search_terms().to_owned();
        for query in [
            "conversation",
            "kokoro",
            "pocket",
            "apple",
            "rate",
            "sensitivity",
            "acceleration",
            "personal",
            "silence",
        ] {
            assert!(terms.contains(query), "{query}");
        }
    }
}

#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
#[test]
fn sensitivity_and_acceleration_pickers_select_the_current_value() {
    use super::{
        WarpAgentPageAction, speaker_sensitivity_options, transcription_acceleration_options,
    };

    let options = speaker_sensitivity_options("loose");
    assert_eq!(
        options.items[options.selected].1,
        WarpAgentPageAction::SetVoiceSpeakerSensitivity("loose")
    );
    assert_eq!(speaker_sensitivity_options("bogus").selected, 1);
    assert_eq!(transcription_acceleration_options("cpu").selected, 1);
    assert_eq!(transcription_acceleration_options("auto").selected, 0);
}
