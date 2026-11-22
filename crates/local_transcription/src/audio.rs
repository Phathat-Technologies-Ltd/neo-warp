use std::io::Cursor;

use crate::TranscribeError;

/// Mono PCM audio with samples in `[-1.0, 1.0]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Decodes a WAV file (integer or float PCM, any channel count) into mono [`Audio`], averaging
/// channels.
pub fn decode_wav(bytes: &[u8]) -> Result<Audio, TranscribeError> {
    let invalid = |error: hound::Error| TranscribeError::InvalidAudio(error.to_string());
    let mut reader = hound::WavReader::new(Cursor::new(bytes)).map_err(invalid)?;
    let spec = reader.spec();
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(invalid)?,
        hound::SampleFormat::Int => {
            let scale = (1_i64 << (spec.bits_per_sample.saturating_sub(1))) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|sample| sample as f32 / scale))
                .collect::<Result<_, _>>()
                .map_err(invalid)?
        }
    };
    let channels = usize::from(spec.channels.max(1));
    let samples = interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect();
    Ok(Audio {
        samples,
        sample_rate: spec.sample_rate,
    })
}

#[cfg(test)]
#[path = "audio_tests.rs"]
mod tests;
