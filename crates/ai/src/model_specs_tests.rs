use super::*;

fn model(
    slug: &str,
    name: &str,
    intelligence: f32,
    tps: f32,
    input: f32,
    output: f32,
) -> BenchmarkedModel {
    BenchmarkedModel {
        slug: slug.to_owned(),
        name: name.to_owned(),
        intelligence: Some(intelligence),
        output_tokens_per_second: Some(tps),
        price_1m_input_tokens: Some(input),
        price_1m_output_tokens: Some(output),
    }
}

fn catalog() -> Catalog {
    Catalog::new(vec![
        model("claude-opus-5-5", "Claude Opus 5.5", 70., 60., 5., 25.),
        model(
            "claude-opus-5-5-thinking",
            "Claude Opus 5.5 (Thinking)",
            74.,
            55.,
            5.,
            25.,
        ),
        model("claude-opus-5", "Claude Opus 5", 65., 50., 15., 75.),
        model(
            "gpt-6-2-sol-xhigh",
            "GPT-6.2 Sol (xhigh)",
            80.,
            90.,
            2.,
            10.,
        ),
        model("gpt-6-2-sol-low", "GPT-6.2 Sol (low)", 60., 120., 2., 10.),
        model("gpt-6", "GPT-6", 50., 100., 1., 4.),
        model(
            "qwen3-coder-480b-a35b-instruct",
            "Qwen3 Coder 480B A35B",
            55.,
            2000.,
            1.,
            1.,
        ),
        model("llama-4-scout", "Llama 4 Scout", 30., 150., 0.1, 0.3),
        model("glm-5-3", "GLM-5.3", 40., 40., 1., 4.),
        model("muse-spark-1-3", "Muse Spark 1.3", 45., 40., 1., 4.),
        model("gpt-5-6-luna", "GPT-5.6 Luna", 35., 40., 1., 4.),
    ])
}

fn scores(id: &str) -> Option<Scores> {
    catalog().scores_for(id, "", &HashMap::new())
}

#[test]
fn matches_variants_but_never_a_different_version() {
    let opus = scores("claude-opus-5-5").unwrap();
    assert_eq!(opus.quality, 70. / 80., "an exact name beats a variant");
    assert_eq!(
        scores("gpt-6.2-sol").unwrap().quality,
        1.,
        "the strongest variant wins a tie"
    );
    assert_eq!(scores("gpt-6").unwrap().quality, 50. / 80.);
    assert!(scores("gpt-6.1-sol").unwrap().estimated_from.is_some());
    assert_eq!(opus.estimated_from, None);
    assert!(scores("claude-sonnet-5-5").is_none());
}

#[test]
fn ignores_size_tags_and_glued_versions() {
    assert_eq!(scores("qwen-3-coder-480b").unwrap().quality, 55. / 80.);
    assert_eq!(
        scores("llama-4-scout-17b-16e-instruct").unwrap().quality,
        30. / 80.
    );
}

#[test]
fn falls_back_to_the_display_name_and_honors_aliases() {
    let catalog = catalog();
    let none = HashMap::new();
    assert!(
        catalog
            .scores_for("chatgpt:odd-id", "Claude Opus 5", &none)
            .is_some()
    );
    let aliases = HashMap::from([("odd-id".to_owned(), "gpt-6-2-sol-low".to_owned())]);
    assert_eq!(
        catalog.scores_for("odd-id", "", &aliases).unwrap().quality,
        60. / 80.
    );
}

#[test]
fn speed_and_cost_rank_models_instead_of_dividing_by_the_extreme() {
    // Qwen is 13x faster than the next model; ranking keeps the rest spread out.
    let qwen = scores("qwen-3-coder-480b").unwrap();
    let scout = scores("llama-4-scout").unwrap();
    let gpt = scores("gpt-6").unwrap();
    assert!(qwen.speed > scout.speed && scout.speed > gpt.speed);
    assert!(
        scout.speed - gpt.speed > 0.1,
        "{} vs {}",
        scout.speed,
        gpt.speed
    );
    assert!(scout.cost < scores("claude-opus-5").unwrap().cost);
    assert_eq!(percentile(1., &[]), 0.05);
    assert_eq!(percentile(5., &[1., 5., 9.]), 0.5);
}

#[test]
fn parses_the_free_endpoint() {
    let body = r#"{
        "tier": "free",
        "pagination": { "page": 1, "has_more": false },
        "data": [{
            "id": "x", "name": "GLM-5.3 (Max)", "slug": "glm-5-3",
            "model_creator": { "id": "y", "name": "Z AI" },
            "evaluations": { "artificial_analysis_intelligence_index": 31.7 },
            "pricing": { "price_1m_input_tokens": 1.4, "price_1m_output_tokens": 4.4 },
            "performance": { "median_output_tokens_per_second": 80.07 }
        }]
    }"#;
    let page: FreeModelsPage = serde_json::from_str(body).unwrap();
    let model = BenchmarkedModel::from(page.data.into_iter().next().unwrap());
    assert_eq!(model.slug, "glm-5-3");
    assert_eq!(model.intelligence, Some(31.7));
    assert_eq!(model.output_tokens_per_second, Some(80.07));
}

fn estimated_from(id: &str) -> Option<String> {
    scores(id).and_then(|scores| scores.estimated_from)
}

#[test]
fn strips_a_vendor_prefix_as_an_estimate() {
    assert_eq!(estimated_from("zai-glm-5.3").as_deref(), Some("GLM-5.3"));
    assert_eq!(scores("zai-glm-5.3").unwrap().quality, 40. / 80.);
    assert_eq!(scores("glm-5.3").unwrap().estimated_from, None);
}

#[test]
fn estimates_from_the_nearest_version_of_the_same_family() {
    assert_eq!(
        estimated_from("msl-muse-spark-1.2").as_deref(),
        Some("Muse Spark 1.3")
    );
    assert_eq!(
        estimated_from("gpt-5.6-sol").as_deref(),
        Some("GPT-6.2 Sol (xhigh)"),
        "never the Luna model of the same version"
    );
    assert_eq!(
        estimated_from("gpt-6.2-luna").as_deref(),
        Some("GPT-5.6 Luna")
    );
}

#[test]
fn never_estimates_across_families() {
    assert!(scores("claude-sonnet-5-5").is_none());
    assert!(scores("claude-sonnet-6").is_none());
    assert!(scores("mistral-large-2").is_none());
    assert!(scores("muse-2").is_none());
}
