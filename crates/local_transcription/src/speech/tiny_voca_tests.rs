use super::*;

fn vocab() -> HashMap<String, i64> {
    [
        ("<bos>", 1),
        ("<eos>", 2),
        ("<sp>", 3),
        ("<unk>", 4),
        (",", 5),
        (".", 6),
        ("h", 31),
        ("l", 35),
        ("w", 38),
        ("ə", 43),
        ("ɜː", 58),
        ("oʊ", 62),
        ("d", 20),
        ("k", 21),
        ("ɑːɹ", 66),
    ]
    .into_iter()
    .map(|(symbol, id)| (symbol.to_owned(), id))
    .collect()
}

#[test]
fn encodes_words_with_stress_and_frames() {
    let g2p = G2p::from_cmudict("hello HH AH0 L OW1\nworld W ER1 L D\n");
    let (ids, stress) = encode(&g2p.convert("Hello, world"), &vocab());
    assert_eq!(ids, vec![1, 31, 43, 35, 62, 5, 3, 38, 58, 35, 20, 6, 2]);
    assert_eq!(stress, vec![0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0]);
}

#[test]
fn merges_r_colored_vowels() {
    let g2p = G2p::from_cmudict("car K AA1 R\n");
    let (ids, stress) = encode(&g2p.convert("car."), &vocab());
    assert_eq!(ids, vec![1, 21, 66, 6, 3, 2]);
    assert_eq!(stress, vec![0, 0, 1, 0, 0, 0]);
}

/// Synthesizes with the real model under `LOCAL_TTS_MODEL_ROOT`.
#[test]
#[ignore = "needs the downloaded model"]
fn synthesizes_with_the_real_model() {
    let root = std::env::var("LOCAL_TTS_MODEL_ROOT").expect("LOCAL_TTS_MODEL_ROOT");
    let speaker = TinyVocaSpeaker::new(ModelStore::new(&root, crate::tiny_voca()));
    assert!(speaker.is_ready());
    let started = instant::Instant::now();
    let audio = speaker
        .synthesize(
            "I want to run cargo test. Say yes or no.",
            &VoiceId::new("F2"),
            1.0,
        )
        .expect("synthesizes");
    eprintln!("{} samples in {:?}", audio.samples.len(), started.elapsed());
    assert!(audio.samples.len() > 24_000);
}
