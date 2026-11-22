use std::sync::atomic::AtomicBool;

use super::*;
use crate::{decode_wav, parakeet_tdt_0_6b_v3};

/// Downloads the real model (~670 MB) into `PARAKEET_MODEL_ROOT` and transcribes the 16 kHz WAV
/// at `PARAKEET_TEST_WAV`, printing the text. Run with `--run-ignored only`.
#[tokio::test]
#[ignore = "downloads the 670 MB model"]
async fn transcribes_real_audio() {
    let root = std::env::var("PARAKEET_MODEL_ROOT").expect("set PARAKEET_MODEL_ROOT");
    let wav = std::env::var("PARAKEET_TEST_WAV").expect("set PARAKEET_TEST_WAV");
    let store = ModelStore::new(root, parakeet_tdt_0_6b_v3());
    store
        .download(&reqwest::Client::new(), &AtomicBool::new(false), |_| {})
        .await
        .unwrap();
    let transcriber = ParakeetTranscriber::new(store);
    assert!(transcriber.is_ready());
    let audio = decode_wav(&std::fs::read(wav).unwrap()).unwrap();
    let text = transcriber.transcribe(&audio, Some("en")).unwrap();
    println!("transcript: {text}");
    assert!(!text.is_empty());
}

#[test]
fn reports_missing_model() {
    let root = tempfile::tempdir().unwrap();
    let transcriber =
        ParakeetTranscriber::new(ModelStore::new(root.path(), parakeet_tdt_0_6b_v3()));
    assert!(!transcriber.is_ready());
    let audio = Audio {
        samples: vec![0.0; 16000],
        sample_rate: 16000,
    };
    assert!(matches!(
        transcriber.transcribe(&audio, None),
        Err(TranscribeError::ModelNotInstalled)
    ));
}
