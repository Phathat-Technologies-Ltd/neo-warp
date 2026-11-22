use super::*;

fn tone(freq: f32, seconds: f32) -> Vec<f32> {
    (0..(SAMPLE_RATE * seconds) as usize)
        .map(|i| 0.5 * (2.0 * PI * freq * i as f32 / SAMPLE_RATE).sin())
        .collect()
}

#[test]
fn frame_count_follows_snip_edges() {
    assert_eq!(compute_fbank(&[0.0; 399]).frames, 0);
    assert_eq!(compute_fbank(&[0.0; 400]).frames, 1);
    let fbank = compute_fbank(&tone(440.0, 1.0));
    assert_eq!(fbank.frames, 98);
    assert_eq!(fbank.data.len(), 98 * FBANK_BINS);
    assert!(fbank.data.iter().all(|value| value.is_finite()));
}

#[test]
fn a_tone_peaks_in_the_matching_mel_bin() {
    let low = compute_fbank(&tone(300.0, 0.5));
    let high = compute_fbank(&tone(4000.0, 0.5));
    let peak = |fbank: &Fbank| {
        let frame = &fbank.data[..FBANK_BINS];
        (0..FBANK_BINS)
            .max_by(|a, b| frame[*a].total_cmp(&frame[*b]))
            .unwrap()
    };
    assert!(peak(&low) < 20, "{}", peak(&low));
    assert!(peak(&high) > 50, "{}", peak(&high));
}

#[test]
fn mean_normalization_cancels_gain() {
    // Noise rather than a tone, so no bin sits at the energy floor, where gain doesn't scale it.
    let mut seed = 7u32;
    let quiet: Vec<f32> = (0..8000)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        })
        .collect();
    let loud: Vec<f32> = quiet.iter().map(|sample| sample * 1.9).collect();
    let mut a = compute_fbank(&quiet);
    let mut b = compute_fbank(&loud);
    a.subtract_mean();
    b.subtract_mean();
    for (x, y) in a.data.iter().zip(&b.data) {
        assert!((x - y).abs() < 1e-3, "{x} vs {y}");
    }
}

#[test]
fn fft_matches_a_direct_dft() {
    let tables = tables();
    let mut re = [0.0f32; FFT_SIZE];
    let mut im = [0.0f32; FFT_SIZE];
    for (i, value) in re.iter_mut().enumerate() {
        *value = ((i * 7) % 13) as f32 - 6.0;
    }
    let input = re;
    fft(&mut re, &mut im, &tables.twiddles);
    for k in [0, 1, 5, 100, 255] {
        let (mut sr, mut si) = (0.0f64, 0.0f64);
        for (n, x) in input.iter().enumerate() {
            let angle = -2.0 * std::f64::consts::PI * (k * n) as f64 / FFT_SIZE as f64;
            sr += *x as f64 * angle.cos();
            si += *x as f64 * angle.sin();
        }
        assert!((re[k] as f64 - sr).abs() < 1e-2, "bin {k}");
        assert!((im[k] as f64 - si).abs() < 1e-2, "bin {k}");
    }
}
