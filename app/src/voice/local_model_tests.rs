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
    let message = progress_message(DownloadProgress {
        downloaded: 335_239_971,
        total: 670_479_942,
    });
    assert_eq!(
        message,
        "Downloading the speech model: 50% (336 MB of 671 MB)"
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
