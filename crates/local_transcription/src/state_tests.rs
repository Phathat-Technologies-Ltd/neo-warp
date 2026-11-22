use super::*;

const PROGRESS: DownloadProgress = DownloadProgress {
    downloaded: 5,
    total: 10,
};

fn downloading() -> ModelState {
    ModelState::Downloading(DownloadProgress {
        downloaded: 0,
        total: 0,
    })
}

#[test]
fn recording_without_model_asks_then_downloads_on_consent() {
    let mut state = ModelState::from_status(ModelStatus::NotInstalled);
    assert_eq!(
        state.apply(ModelEvent::RecordingRequested),
        Some(ModelAction::AskConsent)
    );
    assert_eq!(state, ModelState::AwaitingConsent);
    assert_eq!(
        state.apply(ModelEvent::ConsentGiven),
        Some(ModelAction::StartDownload)
    );
    assert_eq!(state.apply(ModelEvent::Progress(PROGRESS)), None);
    assert_eq!(state, ModelState::Downloading(PROGRESS));
    assert_eq!(state.apply(ModelEvent::DownloadFinished), None);
    assert!(state.is_installed());
    assert_eq!(state.apply(ModelEvent::RecordingRequested), None);
}

#[test]
fn declining_stops_asking_but_settings_can_still_download() {
    let mut state = ModelState::NotInstalled;
    state.apply(ModelEvent::RecordingRequested);
    assert_eq!(state.apply(ModelEvent::ConsentDeclined), None);
    assert_eq!(state.apply(ModelEvent::RecordingRequested), None);
    assert_eq!(state, ModelState::Declined);
    assert_eq!(
        state.apply(ModelEvent::DownloadRequested),
        Some(ModelAction::StartDownload)
    );
}

#[test]
fn unanswered_prompt_is_shown_again() {
    let mut state = ModelState::NotInstalled;
    state.apply(ModelEvent::RecordingRequested);
    assert_eq!(
        state.apply(ModelEvent::RecordingRequested),
        Some(ModelAction::AskConsent)
    );
    assert_eq!(
        state.apply(ModelEvent::DownloadRequested),
        Some(ModelAction::StartDownload)
    );
}

#[test]
fn nothing_downloads_without_consent() {
    let mut state = ModelState::NotInstalled;
    assert_eq!(state.apply(ModelEvent::ConsentGiven), None);
    assert_eq!(state, ModelState::NotInstalled);
}

#[test]
fn failure_asks_again_on_next_recording() {
    let mut state = downloading();
    state.apply(ModelEvent::DownloadFailed("offline".to_owned()));
    assert_eq!(state, ModelState::Failed("offline".to_owned()));
    assert_eq!(
        state.apply(ModelEvent::RecordingRequested),
        Some(ModelAction::AskConsent)
    );
}

#[test]
fn cancel_and_remove() {
    let mut state = downloading();
    assert_eq!(
        state.apply(ModelEvent::CancelRequested),
        Some(ModelAction::CancelDownload)
    );
    assert_eq!(state, ModelState::NotInstalled);

    let mut state = ModelState::Installed;
    assert_eq!(
        state.apply(ModelEvent::RemoveRequested),
        Some(ModelAction::DeleteFiles)
    );
    assert_eq!(state, ModelState::NotInstalled);
}

#[test]
fn reload_doesnt_interrupt_a_download() {
    let mut state = downloading();
    state.apply(ModelEvent::Loaded(ModelStatus::NotInstalled));
    assert_eq!(state, downloading());
}
