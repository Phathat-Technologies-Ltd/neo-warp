use super::*;

fn tokenizer() -> Tokenizer {
    let vocab =
        r#"{"<unk>":0,"<0x21>":1,"▁":2,"▁he":3,"llo":4,"▁hello":5,"l":6,"o":7,"h":8,"e":9}"#;
    let scores = r#"{"<unk>":0,"<0x21>":-10,"▁":-1,"▁he":-2,"llo":-2,"▁hello":-3,"l":-5,"o":-5,"h":-5,"e":-5}"#;
    Tokenizer::parse(vocab, scores).unwrap()
}

#[test]
fn encode_picks_the_highest_scoring_segmentation() {
    assert_eq!(tokenizer().encode("hello"), vec![5]);
}

#[test]
fn encode_marks_spaces_and_falls_back_to_bytes() {
    assert_eq!(tokenizer().encode("hello hello!"), vec![5, 5, 1]);
}

#[test]
fn normal_samples_have_unit_spread() {
    let mut normal = Normal::new(7);
    let samples: Vec<f32> = (0..20_000).map(|_| normal.sample()).collect();
    let mean = samples.iter().sum::<f32>() / samples.len() as f32;
    let variance = samples.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / samples.len() as f32;
    assert!(mean.abs() < 0.05, "mean {mean}");
    assert!((variance - 1.0).abs() < 0.05, "variance {variance}");
}
