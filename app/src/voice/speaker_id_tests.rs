use std::sync::atomic::{AtomicUsize, Ordering};

use local_transcription::TranscribeError;

use super::*;

/// Embeds audio as its mean, so loud and quiet clips differ.
struct FakeEmbedder;

impl SpeakerEmbedder for FakeEmbedder {
    fn embed(&mut self, samples: &[f32]) -> Result<Vec<f32>, TranscribeError> {
        let mean = samples.iter().sum::<f32>() / samples.len().max(1) as f32;
        Ok(vec![mean, 1.0])
    }
}

#[test]
fn worker_loads_lazily_and_reloads_after_idle() {
    let loads = Arc::new(AtomicUsize::new(0));
    let counter = loads.clone();
    let worker = EmbedWorker::spawn(
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeEmbedder) as Box<dyn SpeakerEmbedder>)
        }),
        Duration::from_millis(500),
    )
    .unwrap();
    assert_eq!(loads.load(Ordering::SeqCst), 0);
    let embedding = futures::executor::block_on(worker.embed(vec![0.5; 10])).unwrap();
    assert_eq!(embedding, vec![0.5, 1.0]);
    futures::executor::block_on(worker.embed(vec![0.0; 10])).unwrap();
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    std::thread::sleep(Duration::from_millis(1500));
    futures::executor::block_on(worker.embed(vec![0.0; 10])).unwrap();
    assert_eq!(loads.load(Ordering::SeqCst), 2);
}

#[test]
fn worker_reports_load_failures() {
    let worker = EmbedWorker::spawn(
        Box::new(|| Err("no model".to_owned())),
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(
        futures::executor::block_on(worker.embed(vec![0.0; 10])),
        Err("no model".to_owned())
    );
}

#[test]
fn voiceprint_saves_privately_and_loads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("voice").join("voiceprint.json");
    let print = Voiceprint::enroll("m", &[vec![0.6, 0.8]]).unwrap();
    save_voiceprint(&path, &print).unwrap();
    assert_eq!(load_voiceprint(&path), Some(print));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    delete_voiceprint(&path).unwrap();
    assert_eq!(load_voiceprint(&path), None);
    delete_voiceprint(&path).unwrap();
}

#[test]
fn trim_silence_keeps_the_speech_span() {
    let mut samples = vec![0.0; 1600];
    samples.extend(std::iter::repeat_n(0.2, 3200));
    samples.extend(vec![0.0; 1600]);
    let speech = trim_silence(&samples);
    assert_eq!(speech.len(), 3200);
    assert!(speech.iter().all(|s| *s == 0.2));
    assert!(trim_silence(&[0.0; 1000]).is_empty());
}

#[test]
fn without_a_current_voiceprint_nothing_is_enrolled() {
    assert!(!SpeakerProfile::default().is_enrolled());
    let stale = SpeakerProfile {
        voiceprint: Some(Voiceprint::enroll("an-older-model", &[vec![1.0]]).unwrap()),
        ..SpeakerProfile::default()
    };
    assert!(!stale.is_enrolled());
    let current = SpeakerProfile {
        voiceprint: Some(Voiceprint::enroll(&campplus_en_voxceleb().id, &[vec![1.0]]).unwrap()),
        ..SpeakerProfile::default()
    };
    assert!(current.is_enrolled());
}
