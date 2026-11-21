use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;

use super::*;

fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    move |key| map.get(key).cloned()
}

fn config() -> OrganizationSignInConfig {
    OrganizationSignInConfig {
        issuer: "https://login.example/tenant/v2.0".to_owned(),
        client_id: "client-123".to_owned(),
        display_name: Some("Microsoft".to_owned()),
        extra_scopes: vec!["api://warp/.default".to_owned()],
        redirect_port: None,
    }
}

fn metadata(token_endpoint: &str) -> ProviderMetadata {
    ProviderMetadata {
        authorization_endpoint: "https://login.example/tenant/oauth2/v2.0/authorize".to_owned(),
        token_endpoint: token_endpoint.to_owned(),
    }
}

fn query_params(url: &str) -> HashMap<String, String> {
    let query = url.split_once('?').expect("URL should have a query").1;
    serde_urlencoded::from_str(query).expect("query should parse")
}

fn jwt_with_claims(claims: serde_json::Value) -> String {
    let encode = |value: &serde_json::Value| URL_SAFE_NO_PAD.encode(value.to_string());
    format!(
        "{}.{}.signature",
        encode(&serde_json::json!({ "alg": "RS256" })),
        encode(&claims)
    )
}

/// Serves exactly one HTTP request with `status` and a JSON `body`, returning the base URL and
/// a handle resolving to the raw request it received.
fn serve_once(status: &'static str, body: String) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("fake server should bind");
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("fake server should accept");
        let request = read_http_request(&mut stream);
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        request
    });
    (url, handle)
}

fn read_http_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).expect("fake server should read");
        data.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&data).into_owned();
        if let Some((headers, body)) = text.split_once("\r\n\r\n") {
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if body.len() >= content_length {
                return text;
            }
        }
        if n == 0 {
            return text;
        }
    }
}

fn send_browser_redirect(redirect_uri: &str, query: &str) -> JoinHandle<()> {
    let port = redirect_uri
        .trim_start_matches("http://localhost:")
        .trim_end_matches("/callback")
        .to_owned();
    let request = format!("GET /callback?{query} HTTP/1.1\r\nHost: localhost\r\n\r\n");
    std::thread::spawn(move || {
        let mut stream =
            TcpStream::connect(format!("127.0.0.1:{port}")).expect("browser should connect");
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
    })
}

#[test]
fn config_requires_issuer_and_client_id() {
    assert_eq!(
        OrganizationSignInConfig::from_lookup(lookup(&[(ISSUER_ENV, "https://idp")])).unwrap(),
        None
    );
    assert_eq!(
        OrganizationSignInConfig::from_lookup(lookup(&[
            (ISSUER_ENV, "  "),
            (CLIENT_ID_ENV, "client")
        ]))
        .unwrap(),
        None
    );
}

#[test]
fn config_reads_every_setting_from_env() {
    let config = OrganizationSignInConfig::from_lookup(lookup(&[
        (ISSUER_ENV, " https://idp/v2.0 "),
        (CLIENT_ID_ENV, "client"),
        (DISPLAY_NAME_ENV, "Okta"),
        (EXTRA_SCOPES_ENV, "groups  api://x"),
        (REDIRECT_PORT_ENV, "8400"),
    ]))
    .unwrap()
    .expect("config should be present");
    assert_eq!(
        config,
        OrganizationSignInConfig {
            issuer: "https://idp/v2.0".to_owned(),
            client_id: "client".to_owned(),
            display_name: Some("Okta".to_owned()),
            extra_scopes: vec!["groups".to_owned(), "api://x".to_owned()],
            redirect_port: Some(8400),
        }
    );
    assert!(
        OrganizationSignInConfig::from_lookup(lookup(&[
            (ISSUER_ENV, "https://idp"),
            (CLIENT_ID_ENV, "client"),
            (REDIRECT_PORT_ENV, "not-a-port"),
        ]))
        .is_err()
    );
}

#[test]
fn button_label_names_the_provider_when_configured() {
    assert_eq!(config().button_label(), "Sign in with Microsoft");
    let unnamed = OrganizationSignInConfig {
        display_name: None,
        ..config()
    };
    assert_eq!(unnamed.button_label(), "Sign in with your organization");
}

#[test]
fn authorize_url_carries_pkce_state_nonce_and_loopback_redirect() {
    let attempt = SignInAttempt::start(&config(), metadata("https://unused")).unwrap();
    let url = attempt.authorize_url();
    assert!(url.starts_with("https://login.example/tenant/oauth2/v2.0/authorize?"));

    let params = query_params(&url);
    assert_eq!(params["client_id"], "client-123");
    assert_eq!(params["response_type"], "code");
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(
        params["scope"],
        "openid profile email offline_access api://warp/.default"
    );
    assert_eq!(
        params["redirect_uri"],
        format!("http://localhost:{}/callback", attempt.listener.port())
    );
    assert_eq!(params["state"], attempt.secrets.state);
    assert_eq!(params["nonce"], attempt.secrets.nonce);
    assert_eq!(
        params["code_challenge"],
        URL_SAFE_NO_PAD.encode(Sha256::digest(attempt.secrets.verifier.as_bytes()))
    );
}

#[test]
fn nonce_check_accepts_only_this_attempts_token() {
    let token = jwt_with_claims(serde_json::json!({ "nonce": "n1", "sub": "u" }));
    check_nonce(&token, "n1").expect("matching nonce should pass");
    assert!(check_nonce(&token, "n2").is_err());
    assert!(check_nonce(&jwt_with_claims(serde_json::json!({ "sub": "u" })), "n1").is_err());
    assert!(check_nonce("not-a-jwt", "n1").is_err());
}

#[test]
fn full_flow_exchanges_the_code_and_returns_the_id_token() {
    let attempt = SignInAttempt::start(&config(), metadata("http://placeholder")).unwrap();
    let nonce = attempt.secrets.nonce.clone();
    let id_token = jwt_with_claims(serde_json::json!({ "nonce": nonce, "sub": "user-1" }));
    let (token_url, token_request) = serve_once(
        "200 OK",
        serde_json::json!({ "id_token": id_token, "access_token": "at" }).to_string(),
    );
    let attempt = SignInAttempt {
        metadata: metadata(&format!("{token_url}/token")),
        ..attempt
    };

    let params = query_params(&attempt.authorize_url());
    let browser = send_browser_redirect(
        &params["redirect_uri"],
        &format!("code=the-code&state={}", params["state"]),
    );
    let (_release, result) = attempt.finish();
    let returned = futures::executor::block_on(result).expect("sign-in should succeed");
    browser.join().unwrap();

    assert_eq!(returned, id_token);
    let request = token_request.join().unwrap();
    assert!(request.starts_with("POST /token "));
    let form: HashMap<String, String> =
        serde_urlencoded::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "the-code");
    assert_eq!(form["client_id"], "client-123");
    assert_eq!(form["redirect_uri"], params["redirect_uri"]);
    assert!(!form["code_verifier"].is_empty());
}

#[test]
fn provider_access_denied_maps_to_access_denied() {
    let attempt = SignInAttempt::start(&config(), metadata("http://unused")).unwrap();
    let params = query_params(&attempt.authorize_url());
    let browser = send_browser_redirect(
        &params["redirect_uri"],
        "error=access_denied&error_description=AADSTS50105%3A%20not%20assigned",
    );
    let (_release, result) = attempt.finish();
    let error = futures::executor::block_on(result).expect_err("denied sign-in should fail");
    browser.join().unwrap();
    assert!(
        matches!(&error, SignInError::AccessDenied(detail) if detail.contains("not assigned")),
        "unexpected error: {error}"
    );
}

#[test]
fn backend_exchange_returns_the_custom_token() {
    let (url, request) = serve_once(
        "200 OK",
        serde_json::json!({ "custom_token": "ct-1" }).to_string(),
    );
    let token = futures::executor::block_on(exchange_with_backend(&format!("{url}/"), "idt"))
        .expect("exchange should succeed");
    assert_eq!(token, "ct-1");
    let request = request.join().unwrap();
    assert!(request.starts_with("POST /auth/sso/exchange "));
    assert!(request.ends_with(r#"{"id_token":"idt"}"#));
}

#[test]
fn backend_refusal_maps_to_access_denied() {
    let (url, _request) = serve_once("403 Forbidden", "{}".to_owned());
    let error = futures::executor::block_on(exchange_with_backend(&url, "idt"))
        .expect_err("refused exchange should fail");
    assert!(matches!(error, SignInError::AccessDenied(_)));
}

fn write_config_file(contents: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("organization.toml");
    std::fs::write(&path, contents).unwrap();
    (dir, path)
}

#[test]
fn config_file_is_read() {
    let (_dir, path) = write_config_file(
        r#"
server_url = "https://warp.internal.example.com"

[sign_in]
issuer = "https://login.microsoftonline.com/tenant/v2.0"
client_id = "client"
display_name = "Microsoft"
"#,
    );
    let config = OrganizationConfig::from_file(&path).unwrap();
    assert_eq!(
        config.server_url.as_deref(),
        Some("https://warp.internal.example.com")
    );
    let sign_in = config.sign_in.expect("sign-in should be configured");
    assert_eq!(
        sign_in.issuer,
        "https://login.microsoftonline.com/tenant/v2.0"
    );
    assert_eq!(sign_in.client_id, "client");
    assert_eq!(sign_in.button_label(), "Sign in with Microsoft");
    assert!(sign_in.extra_scopes.is_empty());
    assert_eq!(sign_in.redirect_port, None);
}

#[test]
fn missing_config_file_or_table_means_not_configured() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        OrganizationConfig::from_file(&dir.path().join("absent.toml")).unwrap(),
        OrganizationConfig::default()
    );
    let (_dir, path) = write_config_file("# nothing configured\n");
    assert_eq!(
        OrganizationConfig::from_file(&path).unwrap(),
        OrganizationConfig::default()
    );
}

#[test]
fn invalid_config_files_are_errors() {
    for contents in [
        "[sign_in]\nissuer = \"https://idp\"\nclient_id = \"\"\n",
        "[sign_in\n",
        "server_url = \"warp.internal\"\n",
    ] {
        let (_dir, path) = write_config_file(contents);
        assert!(OrganizationConfig::from_file(&path).is_err(), "{contents}");
    }
}

#[test]
fn environment_configures_the_server_url_and_sign_in() {
    let config = OrganizationConfig::from_lookup(lookup(&[
        (SERVER_URL_ENV, "http://127.0.0.1:8090"),
        (ISSUER_ENV, "https://idp"),
        (CLIENT_ID_ENV, "client"),
    ]))
    .unwrap();
    assert_eq!(config.server_url.as_deref(), Some("http://127.0.0.1:8090"));
    assert_eq!(config.sign_in.unwrap().client_id, "client");
    assert_eq!(
        OrganizationConfig::from_lookup(lookup(&[])).unwrap(),
        OrganizationConfig::default()
    );
}

#[test]
fn shipped_example_config_is_valid() {
    let (_dir, path) = write_config_file(include_str!("../organization.toml.example"));
    let config = OrganizationConfig::from_file(&path).expect("the example must parse");
    assert_eq!(config.server_url, None, "the example runs the embedded server");
    let sign_in = config.sign_in.expect("the example configures sign-in");
    assert_eq!(sign_in.button_label(), "Sign in with Microsoft");
}
