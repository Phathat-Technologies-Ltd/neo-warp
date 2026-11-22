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
        "25%"
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
