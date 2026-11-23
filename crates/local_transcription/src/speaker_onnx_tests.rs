use std::path::PathBuf;

use instant::Instant;

use super::*;
use crate::speaker::{MAX_EMBED_SAMPLES, cosine_similarity};

fn fake_model() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/fake_speaker_embedder.onnx")
}

fn tone(freq: f32, samples: usize) -> Vec<f32> {
    (0..samples)
        .map(|i| 0.3 * (2.0 * std::f32::consts::PI * freq * i as f32 / 16_000.0).sin())
        .collect()
}

/// The fake model averages the features over time, so with mean normalization its raw output is
/// zero; what it checks is that features of the right shape reach the model and a vector of the
/// model's dimension comes back.
#[test]
fn embeds_with_the_model_dimension() {
    let mut embedder = OnnxSpeakerEmbedder::load_file(&fake_model()).unwrap();
    let embedding = embedder.embed(&tone(440.0, 8_000)).unwrap();
    assert_eq!(embedding.len(), FBANK_BINS);
    assert!(embedding.iter().all(|value| value.is_finite()));
    let long = embedder.embed(&tone(440.0, MAX_EMBED_SAMPLES * 2)).unwrap();
    assert_eq!(long.len(), FBANK_BINS);
}

#[test]
fn empty_audio_is_rejected() {
    let mut embedder = OnnxSpeakerEmbedder::load_file(&fake_model()).unwrap();
    assert!(matches!(
        embedder.embed(&[]),
        Err(TranscribeError::InvalidAudio(_))
    ));
}

#[test]
fn missing_model_is_not_installed() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path(), crate::campplus_zh_en_advanced());
    assert!(matches!(
        OnnxSpeakerEmbedder::load(&store),
        Err(TranscribeError::ModelNotInstalled)
    ));
}

/// Measures the real model on WAV files. Set `SPEAKER_BENCH_DIR` to a directory with
/// `campplus_zh_en_advanced_16k.onnx` and 16 kHz WAVs named `<speaker>_<n>.wav`, then run with
/// `--ignored --nocapture`.
#[test]
#[ignore]
fn benchmark_real_model() {
    let Ok(dir) = std::env::var("SPEAKER_BENCH_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let started = Instant::now();
    let mut embedder = OnnxSpeakerEmbedder::load_file(&dir.join(SPEAKER_MODEL_FILE)).unwrap();
    println!("load: {:?}", started.elapsed());
    let mut wavs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "wav"))
        .collect();
    wavs.sort();
    let mut embeddings = Vec::new();
    for path in &wavs {
        let audio = crate::decode_wav(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(audio.sample_rate, 16_000);
        let started = Instant::now();
        let embedding = embedder.embed(&audio.samples).unwrap();
        println!(
            "{}: {:.2}s audio, embed {:?}, dim {}",
            path.file_name().unwrap().to_string_lossy(),
            audio.samples.len() as f32 / 16_000.0,
            started.elapsed(),
            embedding.len()
        );
        embeddings.push(embedding);
    }
    let speaker = |path: &PathBuf| {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        name.split('_').next().unwrap().to_owned()
    };
    for i in 0..wavs.len() {
        for j in i + 1..wavs.len() {
            let same = speaker(&wavs[i]) == speaker(&wavs[j]);
            println!(
                "{} {} vs {}: {:.3}",
                if same { "same" } else { "diff" },
                wavs[i].file_stem().unwrap().to_string_lossy(),
                wavs[j].file_stem().unwrap().to_string_lossy(),
                cosine_similarity(&embeddings[i], &embeddings[j])
            );
        }
    }
    // Leaves time to read the resident size of the loaded model, e.g. with `ps -o rss`.
    if let Ok(seconds) = std::env::var("SPEAKER_BENCH_HOLD_SECS") {
        std::thread::sleep(std::time::Duration::from_secs(seconds.parse().unwrap()));
    }
    drop(embedder);
}
