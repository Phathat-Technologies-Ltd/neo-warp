//! Whisper's log mel spectrogram, matching Hugging Face's `WhisperFeatureExtractor`: a centered,
//! reflect-padded 400-point STFT with a periodic Hann window every 10 ms, 80 Slaney mel bins from
//! 0 to 8 kHz, `log10`, clamped to 8 below the maximum and scaled by `(x + 4) / 4`.

use std::f32::consts::PI;
use std::sync::{Arc, OnceLock};

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

/// Mel bins per frame.
pub const WHISPER_MEL_BINS: usize = 80;
const SAMPLE_RATE: f64 = 16_000.0;
const N_FFT: usize = 400;
const HOP: usize = 160;
const SPECTRUM_BINS: usize = N_FFT / 2 + 1;

struct Tables {
    window: Vec<f32>,
    /// `WHISPER_MEL_BINS` rows of `SPECTRUM_BINS` weights.
    filters: Vec<f32>,
    fft: Arc<dyn Fft<f32>>,
}

/// Computes features of 16 kHz mono `samples`, mel-bin-major (`WHISPER_MEL_BINS` rows of
/// `samples.len() / 160` frames), as Whisper encoders take them.
pub fn whisper_log_mel(samples: &[f32]) -> Vec<f32> {
    let frames = samples.len() / HOP;
    let mut mel = vec![0.0f32; WHISPER_MEL_BINS * frames];
    if frames == 0 {
        return mel;
    }
    let tables = tables();
    let len = samples.len() as isize;
    let reflect = |index: isize| -> f32 {
        let mut index = index;
        if len == 1 {
            return samples[0];
        }
        while index < 0 || index >= len {
            if index < 0 {
                index = -index;
            }
            if index >= len {
                index = 2 * (len - 1) - index;
            }
        }
        samples[index as usize]
    };
    let mut buffer = vec![Complex32::new(0.0, 0.0); N_FFT];
    let mut scratch = vec![Complex32::new(0.0, 0.0); tables.fft.get_inplace_scratch_len()];
    let mut power = [0.0f32; SPECTRUM_BINS];
    let mut max = f32::NEG_INFINITY;
    for frame in 0..frames {
        let start = (frame * HOP) as isize - (N_FFT / 2) as isize;
        for (offset, (value, weight)) in buffer.iter_mut().zip(&tables.window).enumerate() {
            *value = Complex32::new(reflect(start + offset as isize) * weight, 0.0);
        }
        tables.fft.process_with_scratch(&mut buffer, &mut scratch);
        for (power, value) in power.iter_mut().zip(&buffer) {
            *power = value.norm_sqr();
        }
        for bin in 0..WHISPER_MEL_BINS {
            let weights = &tables.filters[bin * SPECTRUM_BINS..(bin + 1) * SPECTRUM_BINS];
            let energy: f32 = weights.iter().zip(&power).map(|(w, p)| w * p).sum();
            let log = energy.max(1e-10).log10();
            max = max.max(log);
            mel[bin * frames + frame] = log;
        }
    }
    let floor = max - 8.0;
    for value in &mut mel {
        *value = (value.max(floor) + 4.0) / 4.0;
    }
    mel
}

/// Scales `samples` to zero mean and unit variance, as `WhisperFeatureExtractor`'s
/// `do_normalize` does.
pub fn normalize(samples: &mut [f32]) {
    if samples.is_empty() {
        return;
    }
    let len = samples.len() as f64;
    let mean = samples.iter().map(|&s| f64::from(s)).sum::<f64>() / len;
    let variance = samples
        .iter()
        .map(|&s| (f64::from(s) - mean).powi(2))
        .sum::<f64>()
        / len;
    let scale = 1.0 / (variance + 1e-7).sqrt();
    for sample in samples {
        *sample = ((f64::from(*sample) - mean) * scale) as f32;
    }
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| Tables {
        window: (0..N_FFT)
            .map(|n| 0.5 - 0.5 * (2.0 * PI * n as f32 / N_FFT as f32).cos())
            .collect(),
        filters: slaney_filters(),
        fft: FftPlanner::new().plan_fft_forward(N_FFT),
    })
}

fn hz_to_mel(hz: f64) -> f64 {
    const MIN_LOG_HZ: f64 = 1000.0;
    const MIN_LOG_MEL: f64 = 15.0;
    let log_step = 27.0 / 6.4f64.ln();
    if hz >= MIN_LOG_HZ {
        MIN_LOG_MEL + (hz / MIN_LOG_HZ).ln() * log_step
    } else {
        3.0 * hz / 200.0
    }
}

fn mel_to_hz(mel: f64) -> f64 {
    const MIN_LOG_HZ: f64 = 1000.0;
    const MIN_LOG_MEL: f64 = 15.0;
    let log_step = 6.4f64.ln() / 27.0;
    if mel >= MIN_LOG_MEL {
        MIN_LOG_HZ * ((mel - MIN_LOG_MEL) * log_step).exp()
    } else {
        200.0 * mel / 3.0
    }
}

/// Triangular filters with Slaney area normalization, like `transformers`' `mel_filter_bank`.
fn slaney_filters() -> Vec<f32> {
    let max_mel = hz_to_mel(SAMPLE_RATE / 2.0);
    let edges: Vec<f64> = (0..WHISPER_MEL_BINS + 2)
        .map(|i| mel_to_hz(max_mel * i as f64 / (WHISPER_MEL_BINS + 1) as f64))
        .collect();
    let mut filters = vec![0.0f32; WHISPER_MEL_BINS * SPECTRUM_BINS];
    for bin in 0..WHISPER_MEL_BINS {
        let (lower, center, upper) = (edges[bin], edges[bin + 1], edges[bin + 2]);
        let norm = 2.0 / (upper - lower);
        for k in 0..SPECTRUM_BINS {
            let hz = k as f64 * SAMPLE_RATE / N_FFT as f64;
            let rising = (hz - lower) / (center - lower);
            let falling = (upper - hz) / (upper - center);
            let weight = rising.min(falling).max(0.0);
            filters[bin * SPECTRUM_BINS + k] = (weight * norm) as f32;
        }
    }
    filters
}

#[cfg(test)]
#[path = "whisper_mel_tests.rs"]
mod tests;
