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
        voiceprint: Some(Voiceprint::enroll(&campplus_zh_en_advanced().id, &[vec![1.0]]).unwrap()),
        ..SpeakerProfile::default()
    };
    assert!(current.is_enrolled());
}

/// A tone at `amplitude` for `speech_secs`, padded with low noise at `noise` on each side.
fn recording(speech_secs: f32, amplitude: f32, noise: f32) -> Vec<f32> {
    let pad = 8_000;
    let speech = (speech_secs * 16_000.0) as usize;
    (0..pad * 2 + speech)
        .map(|i| {
            let hiss = if i % 2 == 0 { noise } else { -noise };
            if (pad..pad + speech).contains(&i) {
                amplitude * (i as f32 * 0.07).sin() + hiss
            } else {
                hiss
            }
        })
        .collect()
}

#[test]
fn quality_accepts_clear_speech() {
    let quality = PhraseQuality::measure(&recording(5.0, 0.2, 0.001));
    assert!((quality.speech_secs - 5.0).abs() < 0.05);
    assert!(quality.speech_dbfs > -20.0 && quality.speech_dbfs < -15.0);
    assert!(quality.noise_dbfs < -55.0);
    assert_eq!(quality.problem(), None);
}

#[test]
fn quality_rejects_short_quiet_clipped_and_noisy_recordings() {
    assert_eq!(
        PhraseQuality::measure(&recording(1.5, 0.2, 0.001)).problem(),
        Some(QualityProblem::TooShort)
    );
    assert_eq!(
        PhraseQuality::measure(&vec![0.0; 80_000]).problem(),
        Some(QualityProblem::TooShort)
    );
    // Loud enough to pass the silence trim, too quiet for a reliable voiceprint.
    assert_eq!(
        PhraseQuality::measure(&recording(5.0, 0.015, 0.0)).problem(),
        Some(QualityProblem::TooQuiet)
    );
    assert_eq!(
        PhraseQuality::measure(&recording(5.0, 1.5, 0.001)).problem(),
        Some(QualityProblem::Clipped)
    );
    assert_eq!(
        PhraseQuality::measure(&recording(5.0, 0.05, 0.02)).problem(),
        Some(QualityProblem::TooNoisy)
    );
}

#[test]
fn flow_records_reviews_and_accepts_each_phrase() {
    let mut flow = EnrollmentFlow::default();
    assert!(!flow.record());
    flow.start();
    assert_eq!(*flow.state(), EnrollmentState::Ready { phrase: 0 });
    for phrase in 0..ENROLLMENT_PHRASES.len() {
        assert!(
            flow.accept().is_none(),
            "nothing to accept before recording"
        );
        assert!(flow.record());
        assert_eq!(*flow.state(), EnrollmentState::Recording { phrase });
        assert!(flow.take().is_none());
        flow.finish_recording(&recording(5.0, 0.2, 0.001));
        assert!(matches!(flow.state(), EnrollmentState::Review { phrase: p, .. } if *p == phrase));
        assert!(flow.take().is_some_and(|take| take.len() > 70_000));
        let accepted = flow.accept();
        if phrase + 1 < ENROLLMENT_PHRASES.len() {
            assert!(accepted.is_none());
            assert_eq!(*flow.state(), EnrollmentState::Ready { phrase: phrase + 1 });
        } else {
            assert_eq!(
                accepted.map(|phrases| phrases.len()),
                Some(ENROLLMENT_PHRASES.len())
            );
            assert_eq!(*flow.state(), EnrollmentState::Processing);
        }
    }
    flow.finish_processing();
    assert_eq!(*flow.state(), EnrollmentState::Idle);
}

#[test]
fn flow_requires_a_good_take_and_allows_rerecording() {
    let mut flow = EnrollmentFlow::default();
    flow.start();
    flow.record();
    flow.finish_recording(&recording(1.0, 0.2, 0.001));
    assert!(flow.accept().is_none());
    assert!(matches!(
        flow.state(),
        EnrollmentState::Review { phrase: 0, .. }
    ));
    assert!(flow.record());
    flow.finish_recording(&recording(5.0, 0.2, 0.001));
    assert!(flow.accept().is_none());
    assert_eq!(*flow.state(), EnrollmentState::Ready { phrase: 1 });
}

#[test]
fn flow_cancel_discards_everything() {
    let mut flow = EnrollmentFlow::default();
    flow.start();
    flow.record();
    flow.finish_recording(&recording(5.0, 0.2, 0.001));
    flow.accept();
    flow.cancel();
    assert_eq!(*flow.state(), EnrollmentState::Idle);
    assert!(!flow.record());
    flow.start();
    assert_eq!(*flow.state(), EnrollmentState::Ready { phrase: 0 });
}

#[test]
fn adapts_only_on_confident_long_matches() {
    assert!(should_adapt(0.70, 0.52, 2.0));
    assert!(!should_adapt(0.58, 0.52, 2.0));
    assert!(!should_adapt(0.70, 0.52, 1.0));
}
