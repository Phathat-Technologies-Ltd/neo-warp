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
    assert!(scores("gpt-6.1-sol").is_none());
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
fn speed_and_cost_use_a_log_scale_against_the_maximum() {
    let qwen = scores("qwen-3-coder-480b").unwrap();
    assert_eq!(qwen.speed, 1.);
    let scout = scores("llama-4-scout").unwrap();
    assert!(scout.speed > 0.5 && scout.speed < 1., "{}", scout.speed);
    assert!(scout.cost < scores("claude-opus-5").unwrap().cost);
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
