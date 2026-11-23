use super::*;

#[test]
fn cosine_similarity_handles_edge_cases() {
    assert!((cosine_similarity(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-6);
    assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 3.0]).abs() < 1e-6);
    assert!((cosine_similarity(&[1.0, 1.0], &[-1.0, -1.0]) + 1.0).abs() < 1e-6);
    assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    assert_eq!(cosine_similarity(&[1.0], &[1.0, 0.0]), 0.0);
}

#[test]
fn thresholds_order_from_strict_to_loose() {
    assert!(Sensitivity::Strict.threshold() > Sensitivity::Normal.threshold());
    assert!(Sensitivity::Normal.threshold() > Sensitivity::Loose.threshold());
    assert_eq!(Sensitivity::from_setting("strict"), Sensitivity::Strict);
    assert_eq!(Sensitivity::from_setting("loose"), Sensitivity::Loose);
    assert_eq!(Sensitivity::from_setting("bogus"), Sensitivity::Normal);
}

#[test]
fn match_depends_on_sensitivity() {
    let print = Voiceprint::enroll("m", &[vec![1.0, 0.0]]).unwrap();
    // cos = 0.48
    let probe = [0.48, (1.0f32 - 0.48 * 0.48).sqrt()];
    assert!(!print.matches(&probe, Sensitivity::Strict));
    assert!(print.matches(&probe, Sensitivity::Normal));
    assert!(print.matches(&probe, Sensitivity::Loose));
}

#[test]
fn enrollment_averages_normalized_embeddings() {
    // Scale doesn't weight a sample: the long vector counts as much as the short one.
    let print = Voiceprint::enroll(
        "m",
        &[
            vec![10.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0],
            vec![0.0, 0.0, 0.0],
        ],
    )
    .unwrap();
    let expected = std::f32::consts::FRAC_1_SQRT_2;
    assert!((print.embedding[0] - expected).abs() < 1e-5);
    assert!((print.embedding[1] - expected).abs() < 1e-5);
    assert_eq!(print.embedding[2], 0.0);
    assert_eq!(print.model_id, "m");
}

#[test]
fn enrollment_rejects_bad_input() {
    assert_eq!(Voiceprint::enroll("m", &[]), Err(EnrollError::NoSamples));
    assert_eq!(
        Voiceprint::enroll("m", &[vec![1.0], vec![1.0, 2.0]]),
        Err(EnrollError::MismatchedDimensions)
    );
}

#[test]
fn voiceprint_round_trips_through_json() {
    let print = Voiceprint::enroll("m", &[vec![0.3, 0.4]]).unwrap();
    let json = serde_json::to_string(&print).unwrap();
    assert_eq!(serde_json::from_str::<Voiceprint>(&json).unwrap(), print);
}

#[test]
fn embedding_clip_keeps_the_end_and_tiles_short_audio() {
    let long: Vec<f32> = (0..MAX_EMBED_SAMPLES + 10).map(|i| i as f32).collect();
    let clip = embedding_clip(&long);
    assert_eq!(clip.len(), MAX_EMBED_SAMPLES);
    assert_eq!(clip[0], 10.0);

    let clip = embedding_clip(&[1.0, 2.0, 3.0]);
    assert_eq!(clip.len(), MIN_EMBED_SAMPLES);
    assert_eq!(&clip[..4], &[1.0, 2.0, 3.0, 1.0]);

    assert!(embedding_clip(&[]).is_empty());
}

#[test]
fn adaptation_moves_the_voiceprint_and_stays_bounded() {
    let mut print = Voiceprint::enroll("m", &[vec![1.0, 0.0], vec![1.0, 0.0]]).unwrap();
    assert!(print.is_current("m"));
    assert!(!print.is_current("other"));
    print.adapt(&[0.0, 5.0]);
    assert!(print.embedding[1] > 0.0);
    for _ in 0..20 {
        print.adapt(&[0.0, 1.0]);
    }
    assert_eq!(print.adapted.len(), MAX_ADAPTED_EMBEDDINGS);
    // The enrollment keeps its share however long the voiceprint adapts.
    assert!(print.embedding[0] > 0.2);
    print.adapt(&[1.0]);
    assert_eq!(print.adapted.len(), MAX_ADAPTED_EMBEDDINGS);
}

#[test]
fn old_voiceprints_are_not_current() {
    let print: Voiceprint =
        serde_json::from_str(r#"{"model_id":"m","embedding":[1.0,0.0]}"#).unwrap();
    assert!(!print.is_current("m"));
}
