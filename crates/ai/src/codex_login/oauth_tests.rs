use serde_json::json;

use super::*;

#[test]
fn lists_only_the_models_meant_for_the_picker_in_order() {
    let body = json!({
        "models": [
            { "slug": "gpt-6.2-sol", "visibility": "list" },
            { "slug": "codex-auto-review", "visibility": "hide" },
            { "slug": "gpt-6-luna" },
            { "slug": " ", "visibility": "list" },
        ]
    })
    .to_string();
    assert_eq!(parse_models(&body).unwrap(), ["gpt-6.2-sol", "gpt-6-luna"]);
    assert!(parse_models("not json").is_err());
}

#[test]
fn reads_the_chatgpt_account_from_the_token() {
    let claims = json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acct-1" } });
    let token = format!(
        "e30.{}.sig",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    );
    assert_eq!(account_id(&token).as_deref(), Some("acct-1"));
    assert_eq!(account_id("opaque-token"), None);
}
