use super::*;

#[test]
fn downsamples_at_the_right_rate_across_calls() {
    let mut resampler = LinearResampler::new(48_000.0, 16_000.0);
    let input: Vec<f32> = (0..4800).map(|i| i as f32).collect();
    let mut output = Vec::new();
    for chunk in input.chunks(437) {
        output.extend(resampler.process(chunk));
    }
    assert!((1599..=1600).contains(&output.len()), "{}", output.len());
    // A ramp resamples to a ramp with step 3, without seams between calls.
    for pair in output.windows(2) {
        assert!((pair[1] - pair[0] - 3.0).abs() < 1e-3, "{pair:?}");
    }
}

#[test]
fn passes_16k_through_one_sample_behind() {
    let mut resampler = LinearResampler::new(16_000.0, 16_000.0);
    let mut output = resampler.process(&[0.1, 0.2]);
    output.extend(resampler.process(&[0.3]));
    assert_eq!(output, vec![0.1, 0.2]);
}

#[test]
fn chunks_carry_pcm_and_level() {
    let chunk = to_chunk(&[1.0, -1.0]);
    assert_eq!(
        chunk.pcm,
        [32767_i16.to_le_bytes(), (-32767_i16).to_le_bytes()].concat()
    );
    assert!((chunk.level - 1.0).abs() < 1e-6);
}

#[test]
fn integer_samples_downmix_to_mono_floats() {
    let mono = downmix(&[i16::MAX, 0, i16::MIN, i16::MIN], 2);
    assert_eq!(mono.len(), 2);
    assert!((mono[0] - 0.5).abs() < 0.01);
    assert!((mono[1] + 1.0).abs() < 0.01);
}

#[test]
fn meter_position_spans_the_speech_range() {
    assert_eq!(meter_position(0.0), 0.0);
    assert_eq!(meter_position(f32::NAN), 0.0);
    assert_eq!(meter_position(0.0001), 0.0);
    assert_eq!(meter_position(1.0), 1.0);
    let speech = meter_position(0.05);
    assert!(speech > 0.3 && speech < 1.0, "{speech}");
}

#[test]
fn mic_level_rises_fast_and_falls_slowly_to_silence() {
    let level = MicLevel::default();
    level.push(0.1);
    let risen = level.get();
    assert!(risen > 0.4, "{risen}");
    level.push(0.0);
    let falling = level.get();
    assert!(falling < risen && falling > 0.5 * risen, "{falling}");
    for _ in 0..200 {
        level.push(0.0);
    }
    assert_eq!(level.get(), 0.0);
    level.push(0.1);
    level.reset();
    assert_eq!(level.get(), 0.0);
}
