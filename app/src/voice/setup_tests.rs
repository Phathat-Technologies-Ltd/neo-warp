use super::*;

use VoiceComponent::{NeuralEngine, SpeechInput, SpeechOutput};

fn fresh(neural_engine_supported: bool) -> Availability {
    Availability {
        speech_input: Presence::Missing,
        neural_engine: Presence::Missing,
        speech_output: Some(Presence::Missing),
        neural_engine_supported,
        neural_engine_offered: false,
    }
}

#[test]
fn fresh_install_needs_the_shared_speech_models_for_every_entry() {
    for supported in [false, true] {
        let speech: &[VoiceComponent] = if supported {
            &[SpeechInput, NeuralEngine]
        } else {
            &[SpeechInput]
        };
        assert_eq!(
            missing_components(VoiceEntry::PushToTalk, fresh(supported)),
            speech
        );
        assert_eq!(
            missing_components(VoiceEntry::HandsFree, fresh(supported)),
            speech
        );
        let mut conversation = speech.to_vec();
        conversation.push(SpeechOutput);
        assert_eq!(
            missing_components(VoiceEntry::Conversation, fresh(supported)),
            conversation
        );
    }
}

#[test]
fn apple_voice_needs_no_speech_output_download() {
    let availability = Availability {
        speech_output: None,
        ..fresh(false)
    };
    assert_eq!(
        missing_components(VoiceEntry::Conversation, availability),
        [SpeechInput]
    );
}

#[test]
fn installed_speech_models_leave_only_the_voice_for_conversation() {
    let availability = Availability {
        speech_input: Presence::Installed,
        neural_engine: Presence::Installed,
        ..fresh(true)
    };
    assert_eq!(
        missing_components(VoiceEntry::Conversation, availability),
        [SpeechOutput]
    );
    assert!(missing_components(VoiceEntry::HandsFree, availability).is_empty());
    assert!(missing_components(VoiceEntry::PushToTalk, availability).is_empty());
}

#[test]
fn existing_cpu_install_isnt_blocked_on_the_neural_engine_package() {
    let availability = Availability {
        speech_input: Presence::Installed,
        ..fresh(true)
    };
    assert!(missing_components(VoiceEntry::PushToTalk, availability).is_empty());
    assert_eq!(
        missing_components(VoiceEntry::Conversation, availability),
        [SpeechOutput]
    );
}

#[test]
fn interrupted_downloads_resume_with_the_next_prompt() {
    let availability = Availability {
        speech_input: Presence::Installed,
        neural_engine: Presence::Partial,
        speech_output: Some(Presence::Partial),
        neural_engine_supported: true,
        neural_engine_offered: true,
    };
    assert_eq!(
        missing_components(VoiceEntry::Conversation, availability),
        [NeuralEngine, SpeechOutput]
    );
    let partial_speech = Availability {
        speech_input: Presence::Partial,
        ..fresh(false)
    };
    assert_eq!(
        missing_components(VoiceEntry::HandsFree, partial_speech),
        [SpeechInput]
    );
}

#[test]
fn declined_neural_engine_package_isnt_downloaded_again() {
    let availability = Availability {
        neural_engine_offered: true,
        ..fresh(true)
    };
    assert_eq!(
        missing_components(VoiceEntry::HandsFree, availability),
        [SpeechInput]
    );
}

#[test]
fn unsupported_macs_never_get_the_neural_engine_package() {
    let availability = Availability {
        neural_engine: Presence::Partial,
        ..fresh(false)
    };
    assert_eq!(
        missing_components(VoiceEntry::PushToTalk, availability),
        [SpeechInput]
    );
}

fn speech_item(already_downloaded: u64) -> PlanItem {
    PlanItem {
        component: SpeechInput,
        parts: vec![
            ("speech recognition", 600),
            ("voice detection", 100),
            ("end-of-turn detection", 300),
        ],
        already_downloaded,
    }
}

fn voice_item() -> PlanItem {
    PlanItem {
        component: SpeechOutput,
        parts: vec![("the voice", 1000)],
        already_downloaded: 0,
    }
}

#[test]
fn plan_item_names_the_part_being_downloaded() {
    let item = speech_item(0);
    assert_eq!(item.current_part(0), "speech recognition");
    assert_eq!(item.current_part(599), "speech recognition");
    assert_eq!(item.current_part(600), "voice detection");
    assert_eq!(item.current_part(700), "end-of-turn detection");
    assert_eq!(item.current_part(1000), "end-of-turn detection");
}

#[test]
fn progress_counts_only_bytes_still_missing() {
    // Speech recognition (600 bytes) was already on disk, so only 400 + 1000 remain.
    let items = [speech_item(600), voice_item()];
    assert_eq!(remaining_size(&items), 1400);
    assert_eq!(
        overall_progress(&items, &[600, 0]),
        DownloadProgress {
            downloaded: 0,
            total: 1400
        }
    );
    assert_eq!(
        overall_progress(&items, &[1000, 300]),
        DownloadProgress {
            downloaded: 700,
            total: 1400
        }
    );
    assert_eq!(overall_progress(&items, &[1000, 300]).percent(), Some(50));
    assert_eq!(
        overall_progress(&items, &[1000, 1000]),
        DownloadProgress {
            downloaded: 1400,
            total: 1400
        }
    );
}

#[test]
fn progress_ignores_a_bundle_reporting_less_than_was_on_disk() {
    let items = [speech_item(600)];
    assert_eq!(overall_progress(&items, &[0]).downloaded, 0);
    assert_eq!(
        downloaded_bytes(
            &items[0],
            &ModelState::Downloading(DownloadProgress {
                downloaded: 0,
                total: 0
            })
        ),
        600
    );
    assert_eq!(downloaded_bytes(&items[0], &ModelState::Installed), 1000);
    assert_eq!(
        downloaded_bytes(&items[0], &ModelState::Failed("offline".to_owned())),
        600
    );
}

#[test]
fn downloads_run_in_order_and_stop_on_cancel_or_failure() {
    let downloading = ModelState::Downloading(DownloadProgress {
        downloaded: 1,
        total: 2,
    });
    assert_eq!(next_step(2, 0, &downloading), Step::Progress);
    assert_eq!(next_step(2, 0, &ModelState::Installed), Step::StartNext(1));
    assert_eq!(next_step(2, 1, &ModelState::Installed), Step::Done);
    assert_eq!(
        next_step(2, 1, &ModelState::Failed("offline".to_owned())),
        Step::Failed("offline".to_owned())
    );
    assert_eq!(next_step(2, 0, &ModelState::NotInstalled), Step::Cancelled);
    assert_eq!(
        next_step(2, 0, &ModelState::AwaitingConsent),
        Step::Cancelled
    );
}

#[test]
fn auto_start_needs_a_recent_click_in_the_active_window() {
    let window = WindowId::new();
    let other = WindowId::new();
    let soon = Duration::from_secs(60);
    assert!(should_auto_start(
        VoiceEntry::Conversation,
        soon,
        window,
        Some(window)
    ));
    assert!(should_auto_start(
        VoiceEntry::HandsFree,
        soon,
        window,
        Some(window)
    ));
    assert!(!should_auto_start(
        VoiceEntry::PushToTalk,
        soon,
        window,
        Some(window)
    ));
    assert!(!should_auto_start(
        VoiceEntry::Conversation,
        soon,
        window,
        Some(other)
    ));
    assert!(!should_auto_start(
        VoiceEntry::Conversation,
        soon,
        window,
        None
    ));
    assert!(!should_auto_start(
        VoiceEntry::Conversation,
        Duration::from_secs(11 * 60),
        window,
        Some(window)
    ));
}

#[test]
fn prompts_show_only_the_total() {
    assert_eq!(
        consent_prompt(&[SpeechInput, NeuralEngine, SpeechOutput], 2_010_000_000),
        "Voice needs to download speech models (2.0 GB). Download now?"
    );
    assert_eq!(
        consent_prompt(&[SpeechOutput], 96_500_000),
        "Voice conversation needs to download a voice (97 MB). Download now?"
    );
    assert_eq!(
        progress_message(
            &[SpeechInput, SpeechOutput],
            DownloadProgress {
                downloaded: 400_000_000,
                total: 1_600_000_000
            },
            "speech recognition"
        ),
        "Downloading speech models: 25% (400 MB of 1.6 GB), speech recognition"
    );
    assert_eq!(
        progress_message(
            &[SpeechOutput],
            DownloadProgress {
                downloaded: 0,
                total: 0
            },
            "the voice"
        ),
        "Downloading a voice: starting"
    );
}

#[test]
fn real_bundle_sizes() {
    use local_transcription::{
        kokoro_82m_v1_int8, parakeet_coreml_encoder, parakeet_tdt_0_6b_v3, silero_vad_v5,
        smart_turn_v3_2,
    };
    let speech: u64 = [parakeet_tdt_0_6b_v3(), silero_vad_v5(), smart_turn_v3_2()]
        .iter()
        .map(ModelSpec::total_size)
        .sum();
    let voice = kokoro_82m_v1_int8().total_size();
    let neural_engine = parakeet_coreml_encoder().total_size();
    assert_eq!(format_size(speech + voice), "780 MB");
    assert_eq!(format_size(speech + neural_engine + voice), "2.0 GB");
}
