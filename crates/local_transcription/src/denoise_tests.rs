use instant::Instant;

use super::*;

fn pcm(samples: impl IntoIterator<Item = i16>) -> Vec<u8> {
    samples.into_iter().flat_map(i16::to_le_bytes).collect()
}

fn noise(len: usize) -> Vec<i16> {
    let mut seed = 0x1234_5678u32;
    (0..len)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((seed >> 16) as i16) / 8
        })
        .collect()
}

fn rms(bytes: &[u8]) -> f32 {
    let samples: Vec<f32> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])))
        .collect();
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

#[test]
fn disabled_stage_passes_audio_through() {
    let mut stage = DenoiseStage::new(false);
    assert!(!stage.is_enabled());
    let input = pcm(noise(1000));
    assert_eq!(stage.process(input.clone()), input);
}

#[test]
fn enabled_stage_outputs_whole_frames_without_losing_samples() {
    let mut stage = DenoiseStage::new(true);
    let mut total = 0;
    // Chunks that don't align with frames, including an odd one.
    for len in [100, 250, 7, 600] {
        total += stage.process(pcm(noise(len))).len() / 2;
    }
    assert_eq!(total, (957 / FRAME_16K) * FRAME_16K);
}

#[test]
fn white_noise_is_attenuated() {
    let mut stage = DenoiseStage::new(true);
    let input = pcm(noise(16_000 * 2));
    let output = stage.process(input.clone());
    // Skip the first half second while RNNoise adapts.
    let tail = |bytes: &[u8]| rms(&bytes[16_000..]);
    assert!(
        tail(&output) < tail(&input) * 0.5,
        "{} vs {}",
        tail(&output),
        tail(&input)
    );
}

/// Prints the CPU cost of denoising; run with `--ignored --nocapture`.
#[test]
#[ignore]
fn benchmark_cpu() {
    let mut stage = DenoiseStage::new(true);
    let input = pcm(noise(16_000 * 10));
    let started = Instant::now();
    let _ = stage.process(input);
    let elapsed = started.elapsed();
    println!(
        "denoised 10 s in {elapsed:?} ({:.2}% of one core)",
        elapsed.as_secs_f64() * 10.0
    );
}
