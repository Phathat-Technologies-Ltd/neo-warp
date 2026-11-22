//! Background-noise suppression with RNNoise (`nnnoiseless`, pure Rust) for 16 kHz microphone
//! audio. RNNoise runs at 48 kHz, so audio is upsampled 3x, denoised in 10 ms frames, and
//! decimated back; the output lags the input by under one frame.

use nnnoiseless::DenoiseState;

/// 16 kHz samples per RNNoise frame.
const FRAME_16K: usize = DenoiseState::FRAME_SIZE / 3;
const I16_SCALE: f32 = 32768.0;

/// Denoises a 16 kHz mono stream, reusing its buffers between calls.
pub struct Denoiser {
    state: Box<DenoiseState<'static>>,
    input: Vec<f32>,
    output: Vec<f32>,
    previous: f32,
}

impl Default for Denoiser {
    fn default() -> Self {
        Self {
            state: DenoiseState::new(),
            input: Vec::with_capacity(DenoiseState::FRAME_SIZE),
            output: vec![0.0; DenoiseState::FRAME_SIZE],
            previous: 0.0,
        }
    }
}

impl Denoiser {
    /// Denoises little-endian `i16` PCM, appending whole denoised frames to `out`.
    pub fn process_pcm(&mut self, pcm: &[u8], out: &mut Vec<u8>) {
        for pair in pcm.as_chunks::<2>().0 {
            let sample = f32::from(i16::from_le_bytes([pair[0], pair[1]]));
            let step = (sample - self.previous) / 3.0;
            self.input.push(self.previous + step);
            self.input.push(self.previous + 2.0 * step);
            self.input.push(sample);
            self.previous = sample;
            if self.input.len() == DenoiseState::FRAME_SIZE {
                self.state.process_frame(&mut self.output, &self.input);
                self.input.clear();
                for triple in self.output.as_chunks::<3>().0 {
                    let mean = (triple[0] + triple[1] + triple[2]) / 3.0;
                    let sample = mean.clamp(-I16_SCALE, I16_SCALE - 1.0) as i16;
                    out.extend_from_slice(&sample.to_le_bytes());
                }
            }
        }
    }
}

/// The optional denoise stage in front of voice activity detection.
#[derive(Default)]
pub struct DenoiseStage {
    denoiser: Option<Denoiser>,
}

impl DenoiseStage {
    pub fn new(enabled: bool) -> Self {
        Self {
            denoiser: enabled.then(Denoiser::default),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.denoiser.is_some()
    }

    /// Returns `pcm` denoised, or unchanged when the stage is off.
    pub fn process(&mut self, pcm: Vec<u8>) -> Vec<u8> {
        match &mut self.denoiser {
            None => pcm,
            Some(denoiser) => {
                let mut out = Vec::with_capacity(pcm.len() + FRAME_16K * 2);
                denoiser.process_pcm(&pcm, &mut out);
                out
            }
        }
    }
}

#[cfg(test)]
#[path = "denoise_tests.rs"]
mod tests;
