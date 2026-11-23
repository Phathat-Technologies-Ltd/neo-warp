use super::*;

#[test]
fn frames_follow_the_hop() {
    let mel = whisper_log_mel(&vec![0.0; 16_000]);
    assert_eq!(mel.len(), WHISPER_MEL_BINS * 100);
}

#[test]
fn silence_sits_at_the_floor() {
    let mel = whisper_log_mel(&vec![0.0; 3200]);
    assert!(
        mel.iter()
            .all(|&value| (value - (-10.0 + 4.0) / 4.0).abs() < 1e-6)
    );
}

#[test]
fn tone_peaks_in_its_mel_bin() {
    let samples: Vec<f32> = (0..16_000)
        .map(|n| (2.0 * PI * 1000.0 * n as f32 / 16_000.0).sin())
        .collect();
    let mel = whisper_log_mel(&samples);
    let frames = 100;
    let frame = 50;
    let loudest = (0..WHISPER_MEL_BINS)
        .max_by(|&a, &b| mel[a * frames + frame].total_cmp(&mel[b * frames + frame]))
        .unwrap();
    let center_hz = mel_to_hz(hz_to_mel(8000.0) * (loudest + 1) as f64 / 81.0);
    assert!((center_hz - 1000.0).abs() < 100.0, "peak at {center_hz} Hz");
}

#[test]
fn normalize_gives_zero_mean_unit_variance() {
    let mut samples: Vec<f32> = (0..1000).map(|n| (n % 7) as f32).collect();
    normalize(&mut samples);
    let mean = samples.iter().sum::<f32>() / 1000.0;
    let variance = samples.iter().map(|s| (s - mean).powi(2)).sum::<f32>() / 1000.0;
    assert!(mean.abs() < 1e-4);
    assert!((variance - 1.0).abs() < 1e-3);
}
