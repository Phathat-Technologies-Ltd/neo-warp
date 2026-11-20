use super::*;

#[test]
fn authorize_url_contains_pkce_params() {
    let pkce = PkceParams { verifier: "v".into(), challenge: "c".into(), state: "v".into() };
    let url = authorize_url(&pkce);
    assert!(url.contains("code_challenge=c"));
    assert!(url.contains("state=v"));
    assert!(url.contains(CLIENT_ID));
}

#[test]
fn console_authorize_url_uses_console_host() {
    let pkce = PkceParams { verifier: "v".into(), challenge: "c".into(), state: "v".into() };
    let url = authorize_url_for_flow(&pkce, AnthropicOAuthFlow::Console);
    assert!(url.starts_with(CONSOLE_AUTHORIZE_URL));
    assert!(url.contains(CLIENT_ID));
}
