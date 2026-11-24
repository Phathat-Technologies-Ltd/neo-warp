use super::*;

#[test]
fn formats_sizes_and_progress() {
    assert_eq!(format_megabytes(670_479_942), "671 MB");
    assert_eq!(
        format_progress(DownloadProgress {
            downloaded: 0,
            total: 0
        }),
        "starting"
    );
    assert_eq!(
        format_progress(DownloadProgress {
            downloaded: 25,
            total: 100
        }),
        "25% (1 MB of 1 MB)"
    );
    assert_eq!(
        format_progress(DownloadProgress {
            downloaded: 550_000_000,
            total: 670_479_942
        }),
        "82% (550 MB of 671 MB)"
    );
}

#[test]
fn toast_follows_the_download() {
    let progress = DownloadProgress {
        downloaded: 10,
        total: 100,
    };
    let downloading = ModelState::Downloading(progress);
    assert_eq!(
        toast_update(&ModelState::AwaitingConsent, &downloading),
        Some(ToastUpdate::ShowProgress(progress))
    );
    assert_eq!(
        toast_update(&ModelState::Failed("x".to_owned()), &downloading),
        Some(ToastUpdate::ShowProgress(progress))
    );
    assert_eq!(
        toast_update(&downloading, &downloading),
        Some(ToastUpdate::UpdateProgress(progress))
    );
    assert_eq!(
        toast_update(&downloading, &ModelState::Installed),
        Some(ToastUpdate::Ready)
    );
    assert_eq!(
        toast_update(&downloading, &ModelState::Failed("offline".to_owned())),
        Some(ToastUpdate::Failed("offline".to_owned()))
    );
    assert_eq!(
        toast_update(&downloading, &ModelState::NotInstalled),
        Some(ToastUpdate::Dismiss)
    );
    assert_eq!(
        toast_update(&ModelState::Installed, &ModelState::NotInstalled),
        None
    );
}

#[test]
fn progress_toast_shows_live_percent() {
    let message = progress_message(
        "speech model",
        DownloadProgress {
            downloaded: 335_239_971,
            total: 670_479_942,
        },
    );
    assert_eq!(
        message,
        "Downloading the speech model: 50% (336 MB of 671 MB)"
    );
}

#[test]
fn speech_output_engine_defaults_to_kokoro() {
    assert_eq!(
        SpeechOutputEngine::from_setting("tiny_voca"),
        SpeechOutputEngine::TinyVoca
    );
    assert_eq!(
        SpeechOutputEngine::from_setting(""),
        SpeechOutputEngine::Kokoro
    );
    let size = SpeechOutputEngine::Kokoro.spec().unwrap().total_size();
    assert!(
        SpeechOutputKind::consent_prompt("Kokoro 82M", size).contains("(99 MB)"),
        "{size}"
    );
}

#[test]
fn consent_prompt_names_model_and_size() {
    let prompt = consent_prompt("Parakeet TDT 0.6B v3", 670_479_942);
    assert!(prompt.contains("Parakeet TDT 0.6B v3"));
    assert!(prompt.contains("671 MB"));
}

#[test]
fn model_lives_under_the_data_dir() {
    let store = model_store();
    assert!(store.model_dir().starts_with(warp_core::paths::data_dir()));
    assert_eq!(store.spec(), &parakeet_tdt_0_6b_v3());
}

#[test]
fn pocket_engine_round_trips_through_the_setting() {
    for engine in [
        SpeechOutputEngine::Kokoro,
        SpeechOutputEngine::TinyVoca,
        SpeechOutputEngine::Pocket,
    ] {
        assert_eq!(
            SpeechOutputEngine::from_setting(engine.setting_value()),
            engine
        );
    }
    assert_eq!(
        SpeechOutputEngine::from_setting("pocket"),
        SpeechOutputEngine::Pocket
    );
    assert_eq!(
        SpeechOutputEngine::from_setting("unknown"),
        SpeechOutputEngine::Kokoro
    );
    assert_eq!(
        SpeechOutputEngine::Pocket.spec().unwrap().display_name,
        "Pocket TTS"
    );
}

#[test]
fn engine_picker_lists_every_engine_for_the_platform() {
    let mut expected = vec![
        SpeechOutputEngine::Kokoro,
        SpeechOutputEngine::TinyVoca,
        SpeechOutputEngine::Pocket,
    ];
    if cfg!(target_os = "macos") {
        expected.push(SpeechOutputEngine::Apple);
    }
    assert_eq!(SpeechOutputEngine::all(), expected);
    for engine in SpeechOutputEngine::all() {
        assert_eq!(
            SpeechOutputEngine::from_setting(engine.setting_value()),
            engine
        );
    }
}

#[test]
fn failed_engines_fall_back_to_kokoro() {
    assert_eq!(
        SpeechOutputEngine::Pocket.fallback(),
        Some(SpeechOutputEngine::Kokoro)
    );
    assert_eq!(
        SpeechOutputEngine::TinyVoca.fallback(),
        Some(SpeechOutputEngine::Kokoro)
    );
    assert_eq!(SpeechOutputEngine::Kokoro.fallback(), None);
    assert!(!SpeechOutputEngine::Pocket.has_voice_and_rate_controls());
    assert!(SpeechOutputEngine::Kokoro.has_voice_and_rate_controls());
}

#[test]
fn apple_engine_round_trips_through_the_setting() {
    let engine = SpeechOutputEngine::from_setting("apple");
    if cfg!(target_os = "macos") {
        assert_eq!(engine, SpeechOutputEngine::Apple);
        assert_eq!(engine.setting_value(), "apple");
        assert_eq!(engine.spec(), None);
        assert!(speech_output_store(engine).is_none());
        assert_eq!(engine.fallback(), Some(SpeechOutputEngine::Kokoro));
        assert!(engine.has_voice_and_rate_controls());
    } else {
        assert_eq!(engine, SpeechOutputEngine::Kokoro);
    }
}

#[test]
fn offers_the_neural_engine_model_once_on_auto_apple_silicon() {
    use local_transcription::tdt::Acceleration;
    let offer = |acceleration, apple_silicon, installed, state: &ModelState, offered| {
        should_offer_accelerated_speech_model(
            acceleration,
            apple_silicon,
            installed,
            state,
            offered,
        )
    };
    let missing = ModelState::NotInstalled;
    assert!(offer(Acceleration::Auto, true, true, &missing, false));
    assert!(offer(
        Acceleration::Auto,
        true,
        true,
        &ModelState::AwaitingConsent,
        false
    ));
    assert!(!offer(Acceleration::Auto, true, true, &missing, true));
    assert!(!offer(Acceleration::Cpu, true, true, &missing, false));
    assert!(!offer(Acceleration::Auto, false, true, &missing, false));
    assert!(!offer(Acceleration::Auto, true, false, &missing, false));
    for state in [
        ModelState::Installed,
        ModelState::Downloading(DownloadProgress {
            downloaded: 0,
            total: 0,
        }),
        ModelState::Failed("offline".to_owned()),
    ] {
        assert!(!offer(Acceleration::Auto, true, true, &state, false));
    }
}

#[test]
fn neural_engine_model_lands_in_the_speech_model_dir() {
    let store = accelerated_speech_store();
    assert_eq!(store.model_dir(), model_store().model_dir());
    assert_eq!(
        store.spec().files[0].name,
        local_transcription::tdt::FP16_ENCODER_FILE
    );
    assert_eq!(
        AcceleratedSpeechKind::consent_prompt("", store.spec().total_size()),
        "Download the faster Apple Neural Engine speech model (1.2 GB)? Voice input keeps working \
         on the CPU until it's ready."
    );
}
