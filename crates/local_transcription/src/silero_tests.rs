use std::sync::atomic::AtomicBool;

use super::*;
use crate::silero_vad_v5;

#[test]
fn reports_missing_model() {
    let root = tempfile::tempdir().unwrap();
    let store = ModelStore::new(root.path(), silero_vad_v5());
    assert!(matches!(
        SileroVad::load(&store),
        Err(TranscribeError::ModelNotInstalled)
    ));
}

/// Downloads the real model (2 MB) into a temporary directory and checks that silence scores low.
/// Run with `--run-ignored only`.
#[tokio::test]
#[ignore = "downloads the model"]
async fn scores_real_audio() {
    let root = tempfile::tempdir().unwrap();
    let store = ModelStore::new(root.path(), silero_vad_v5());
    store
        .download(&reqwest::Client::new(), &AtomicBool::new(false), |_| {})
        .await
        .unwrap();
    let mut vad = SileroVad::load(&store).unwrap();
    for _ in 0..10 {
        let probability = vad.speech_probability(&[0.0; VAD_FRAME_SAMPLES]).unwrap();
        assert!(probability < 0.2, "{probability}");
    }
}
