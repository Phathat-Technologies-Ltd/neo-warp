use super::*;

#[test]
fn builds_misaki_phonemes_with_stress() {
    let g2p = G2p::from_cmudict("hello HH AH0 L OW1\nworld W ER1 L D\n");
    assert_eq!(
        phoneme_string(&g2p.convert("Hello, world.")),
        "həlˈO, wˈɜɹld."
    );
}

#[test]
fn reads_the_vocab_from_the_tokenizer() {
    let vocab = parse_vocab(r#"{"model":{"vocab":{"$":0,"a":43,"ˈ":156,"ab":9}}}"#).unwrap();
    assert_eq!(vocab.get(&'a'), Some(&43));
    assert_eq!(vocab.get(&'ˈ'), Some(&156));
    assert_eq!(vocab.len(), 3);
}

#[test]
fn every_mapped_symbol_is_in_the_kokoro_vocab() {
    // The single-character symbols of onnx-community/Kokoro-82M-v1.0-ONNX's tokenizer.json.
    let vocab: Vec<char> = "$;:,.!?—…\"()“” AIOQSTWYᵊabcdefhijklmnopqrstuvwxyzɑɐɒæβɔɕçɖðʤəɚɛɜɟɡɥɨɪʝɯɰŋɳɲɴøɸθœɹɾɻʁɽʂʃʈʧʊʋʌɣɤχʎʒʔˈˌːʰʲ↓→↗↘ᵻ".chars().collect();
    for symbol in [
        "AA", "AE", "AH", "AO", "AW", "AY", "EH", "ER", "EY", "IH", "IY", "OW", "OY", "UH", "UW",
        "B", "CH", "D", "DH", "F", "G", "HH", "JH", "K", "L", "M", "N", "NG", "P", "R", "S", "SH",
        "T", "TH", "V", "W", "Y", "Z", "ZH",
    ] {
        for stress in [Some(0), Some(1), None] {
            let mapped = misaki(&Phone {
                symbol: symbol.to_owned(),
                stress,
            });
            assert!(!mapped.is_empty(), "{symbol}");
            for c in mapped.chars() {
                assert!(vocab.contains(&c), "{symbol} maps to {c}, not in the vocab");
            }
        }
    }
}

/// Synthesizes with the real model under `LOCAL_TTS_MODEL_ROOT` (the store root holding the
/// downloaded files), writing `kokoro-smoke.wav` there.
#[test]
#[ignore = "needs the downloaded model"]
fn synthesizes_with_the_real_model() {
    let root = std::env::var("LOCAL_TTS_MODEL_ROOT").expect("LOCAL_TTS_MODEL_ROOT");
    let speaker = KokoroSpeaker::new(ModelStore::new(&root, crate::kokoro_82m_v1_int8()));
    assert!(speaker.is_ready());
    let text = crate::speech::tts_text::normalize(
        "Ran cargo check --release: 2 errors in app/src/lib.rs.",
    );
    let started = instant::Instant::now();
    let audio = speaker
        .synthesize(&text, &VoiceId::new("af_heart"), 1.0)
        .expect("synthesizes");
    eprintln!(
        "{text:?}: {} samples in {:?}",
        audio.samples.len(),
        started.elapsed()
    );
    assert!(audio.samples.len() > 24_000);
    let started = instant::Instant::now();
    speaker
        .synthesize("Say yes or no.", &VoiceId::new("af_heart"), 1.0)
        .expect("synthesizes");
    eprintln!("warm short sentence in {:?}", started.elapsed());
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: audio.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let path = std::path::Path::new(&root).join("kokoro-smoke.wav");
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for sample in &audio.samples {
        writer
            .write_sample((sample.clamp(-1.0, 1.0) * 32767.0) as i16)
            .unwrap();
    }
    writer.finalize().unwrap();
}
