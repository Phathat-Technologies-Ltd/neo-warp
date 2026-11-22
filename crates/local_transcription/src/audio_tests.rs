use std::io::Cursor;

use super::*;

fn wav(channels: u16, samples: &[i16]) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
    for sample in samples {
        writer.write_sample(*sample).unwrap();
    }
    writer.finalize().unwrap();
    cursor.into_inner()
}

#[test]
fn decodes_mono_int16() {
    let audio = decode_wav(&wav(1, &[0, 16384, -32768])).unwrap();
    assert_eq!(audio.sample_rate, 16000);
    assert_eq!(audio.samples, vec![0.0, 0.5, -1.0]);
}

#[test]
fn averages_channels() {
    let audio = decode_wav(&wav(2, &[16384, 0, -16384, -16384])).unwrap();
    assert_eq!(audio.samples, vec![0.25, -0.5]);
}

#[test]
fn rejects_garbage() {
    assert!(matches!(
        decode_wav(b"not a wav"),
        Err(TranscribeError::InvalidAudio(_))
    ));
}
