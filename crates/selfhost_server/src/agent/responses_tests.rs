use super::*;

fn jwt(claims: Value) -> String {
    let encode = |value: &Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap());
    format!(
        "{}.{}.signature",
        encode(&json!({ "alg": "RS256" })),
        encode(&claims)
    )
}

#[test]
fn reads_the_chatgpt_account_from_the_auth_claim() {
    let token = jwt(json!({
        "https://api.openai.com/auth": { "chatgpt_account_id": "acct-123" },
    }));
    assert_eq!(chatgpt_account_id(&token).as_deref(), Some("acct-123"));
    assert_eq!(chatgpt_account_id("not-a-jwt"), None);
}

#[test]
fn recognizes_the_codex_backend() {
    assert!(is_codex_backend("https://chatgpt.com/backend-api/codex"));
    assert!(!is_codex_backend("https://api.openai.com/v1"));
    assert!(!is_codex_backend("not a url"));
}

#[test]
fn moves_the_system_prompt_to_instructions_and_drops_rejected_fields() {
    let mut payload = json!({
        "model": "gpt-6.2-sol",
        "input": [
            { "type": "message", "role": "system", "content": [{ "type": "input_text", "text": "be brief" }] },
            { "role": "user", "content": [{ "type": "input_text", "text": "hi" }] },
        ],
        "store": true,
        "temperature": 0.2,
        "max_output_tokens": 1000,
    });
    shape_payload(&mut payload, "be brief", false, false);
    assert_eq!(payload["instructions"], "be brief");
    assert_eq!(payload["input"].as_array().unwrap().len(), 1);
    assert_eq!(payload["input"][0]["role"], "user");
    assert_eq!(payload["store"], false);
    assert_eq!(payload["stream"], true);
    assert!(payload.get("temperature").is_none());
    assert!(payload.get("max_output_tokens").is_none());
    assert!(
        payload.get("reasoning").is_none(),
        "only the Codex backend gets reasoning defaults"
    );
}

#[test]
fn the_codex_backend_gets_reasoning_replay_and_tool_defaults() {
    let mut payload = json!({ "model": "gpt-6.2-sol", "input": [] });
    shape_payload(&mut payload, "", true, true);
    assert!(payload.get("instructions").is_none());
    assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(payload["reasoning"]["effort"], "medium");
    assert_eq!(payload["tool_choice"], "auto");
    assert_eq!(payload["parallel_tool_calls"], true);

    let mut payload = json!({ "model": "m", "input": [], "reasoning": { "effort": "high" } });
    shape_payload(&mut payload, "", false, true);
    assert_eq!(payload["reasoning"]["effort"], "high");
    assert!(payload.get("tool_choice").is_none());
}
