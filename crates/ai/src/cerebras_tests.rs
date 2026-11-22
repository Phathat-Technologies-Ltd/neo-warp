use super::*;

#[test]
fn parses_the_model_ids_cerebras_lists() {
    let body = r#"{
        "object": "list",
        "data": [
            {"id": "qwen-3-coder-480b", "object": "model", "created": 0, "owned_by": "Qwen"},
            {"id": "llama-4-scout-17b-16e-instruct", "object": "model", "created": 0, "owned_by": "Meta"},
            {"id": "qwen-3-coder-480b", "object": "model", "created": 0, "owned_by": "Qwen"},
            {"id": " ", "object": "model", "created": 0, "owned_by": "nobody"}
        ]
    }"#;
    assert_eq!(
        parse_models(body).unwrap(),
        ["llama-4-scout-17b-16e-instruct", "qwen-3-coder-480b"]
    );
}

#[test]
fn rejects_a_body_that_is_not_a_model_list() {
    assert!(parse_models(r#"{"error": "nope"}"#).is_err());
}

#[test]
fn endpoint_offers_each_model_under_a_cerebras_config_key() {
    let endpoint = endpoint("csk-test", &["qwen-3-coder-480b".to_owned()]);
    assert_eq!(endpoint.url, BASE_URL);
    assert_eq!(endpoint.api_key, "csk-test");
    assert_eq!(endpoint.schema, CustomEndpointSchema::OpenaiChatCompletions);
    assert_eq!(endpoint.models.len(), 1);
    assert_eq!(endpoint.models[0].name, "qwen-3-coder-480b");
    assert!(is_cerebras_config_key(&endpoint.models[0].config_key));
    assert!(!is_cerebras_config_key(
        "6f1c2a52-6a3e-4c8e-9b8e-0a7f1f5c2d11"
    ));
}
