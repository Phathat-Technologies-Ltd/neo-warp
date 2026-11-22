//! Kaldi-compatible log mel filterbank features, as speaker embedding models trained with Kaldi,
//! torchaudio's `compliance.kaldi.fbank`, or kaldi-native-fbank expect: 25 ms Povey-windowed
//! frames every 10 ms, pre-emphasis 0.97, DC removal, no dither, `snip_edges`, 80 mel bins from
//! 20 Hz to Nyquist.

use std::f32::consts::PI;
use std::sync::OnceLock;

/// Mel bins per frame.
pub const FBANK_BINS: usize = 80;
const SAMPLE_RATE: f32 = 16_000.0;
const FRAME_LENGTH: usize = 400;
const FRAME_SHIFT: usize = 160;
const FFT_SIZE: usize = 512;
const PREEMPHASIS: f32 = 0.97;
const LOW_FREQ: f32 = 20.0;

/// Features of 16 kHz audio, [`FBANK_BINS`] per frame, frames concatenated.
#[derive(Debug, Clone, PartialEq)]
pub struct Fbank {
    pub frames: usize,
    pub data: Vec<f32>,
}

impl Fbank {
    /// Subtracts each bin's mean over all frames (cepstral mean normalization).
    pub fn subtract_mean(&mut self) {
        if self.frames == 0 {
            return;
        }
        let mut mean = [0.0f32; FBANK_BINS];
        for frame in self.data.as_chunks::<FBANK_BINS>().0 {
            for (sum, value) in mean.iter_mut().zip(frame) {
                *sum += value;
            }
        }
        let frames = self.frames as f32;
        mean.iter_mut().for_each(|sum| *sum /= frames);
        for frame in self.data.as_chunks_mut::<FBANK_BINS>().0 {
            for (value, mean) in frame.iter_mut().zip(&mean) {
                *value -= mean;
            }
        }
    }
}

/// Computes log mel filterbank features of 16 kHz mono `samples`. Audio shorter than one frame
/// (25 ms) has no frames.
pub fn compute_fbank(samples: &[f32]) -> Fbank {
    if samples.len() < FRAME_LENGTH {
        return Fbank {
            frames: 0,
            data: Vec::new(),
        };
    }
    let frames = 1 + (samples.len() - FRAME_LENGTH) / FRAME_SHIFT;
    let tables = tables();
    let mut data = Vec::with_capacity(frames * FBANK_BINS);
    let mut frame = [0.0f32; FRAME_LENGTH];
    let mut re = [0.0f32; FFT_SIZE];
    let mut im = [0.0f32; FFT_SIZE];
    let mut power = [0.0f32; FFT_SIZE / 2];
    for index in 0..frames {
        let start = index * FRAME_SHIFT;
        frame.copy_from_slice(&samples[start..start + FRAME_LENGTH]);
        let mean = frame.iter().sum::<f32>() / FRAME_LENGTH as f32;
        frame.iter_mut().for_each(|sample| *sample -= mean);
        for i in (1..FRAME_LENGTH).rev() {
            frame[i] -= PREEMPHASIS * frame[i - 1];
        }
        frame[0] -= PREEMPHASIS * frame[0];
        re.fill(0.0);
        im.fill(0.0);
        for (out, (sample, weight)) in re.iter_mut().zip(frame.iter().zip(&tables.window)) {
            *out = sample * weight;
        }
        fft(&mut re, &mut im, &tables.twiddles);
        for (bin, out) in power.iter_mut().enumerate() {
            *out = re[bin] * re[bin] + im[bin] * im[bin];
        }
        for filter in &tables.filters {
            let energy: f32 = filter
                .weights
                .iter()
                .zip(&power[filter.first_bin..])
                .map(|(weight, power)| weight * power)
                .sum();
            data.push(energy.max(f32::EPSILON).ln());
        }
    }
    Fbank { frames, data }
}

struct MelFilter {
    first_bin: usize,
    weights: Vec<f32>,
}

struct Tables {
    window: Vec<f32>,
    filters: Vec<MelFilter>,
    twiddles: Vec<(f32, f32)>,
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let window = (0..FRAME_LENGTH)
            .map(|i| {
                let hann = 0.5 - 0.5 * (2.0 * PI * i as f32 / (FRAME_LENGTH - 1) as f32).cos();
                hann.powf(0.85)
            })
            .collect();
        let mel = |freq: f32| 1127.0 * (1.0 + freq / 700.0).ln();
        let mel_low = mel(LOW_FREQ);
        let mel_high = mel(SAMPLE_RATE / 2.0);
        let delta = (mel_high - mel_low) / (FBANK_BINS + 1) as f32;
        let bin_width = SAMPLE_RATE / FFT_SIZE as f32;
        let filters = (0..FBANK_BINS)
            .map(|bin| {
                let left = mel_low + bin as f32 * delta;
                let center = left + delta;
                let right = center + delta;
                let weights: Vec<(usize, f32)> = (0..FFT_SIZE / 2)
                    .filter_map(|fft_bin| {
                        let m = mel(bin_width * fft_bin as f32);
                        (m > left && m < right).then(|| {
                            let weight = if m <= center {
                                (m - left) / (center - left)
                            } else {
                                (right - m) / (right - center)
                            };
                            (fft_bin, weight)
                        })
                    })
                    .collect();
                MelFilter {
                    first_bin: weights.first().map_or(0, |(fft_bin, _)| *fft_bin),
                    weights: weights.into_iter().map(|(_, weight)| weight).collect(),
                }
            })
            .collect();
        let twiddles = (0..FFT_SIZE / 2)
            .map(|k| {
                let angle = -2.0 * PI * k as f32 / FFT_SIZE as f32;
                (angle.cos(), angle.sin())
            })
            .collect();
        Tables {
            window,
            filters,
            twiddles,
        }
    })
}

/// In-place iterative radix-2 FFT of length `FFT_SIZE`.
fn fft(re: &mut [f32; FFT_SIZE], im: &mut [f32; FFT_SIZE], twiddles: &[(f32, f32)]) {
    let bits = FFT_SIZE.trailing_zeros();
    for i in 0..FFT_SIZE {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= FFT_SIZE {
        let half = len / 2;
        let step = FFT_SIZE / len;
        for start in (0..FFT_SIZE).step_by(len) {
            for k in 0..half {
                let (wr, wi) = twiddles[k * step];
                let (a, b) = (start + k, start + k + half);
                let tr = re[b] * wr - im[b] * wi;
                let ti = re[b] * wi + im[b] * wr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len *= 2;
    }
}

#[cfg(test)]
#[path = "fbank_tests.rs"]
mod tests;
