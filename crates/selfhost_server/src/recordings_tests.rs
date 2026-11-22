use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request};
use serde_json::{Value, json};
use session_recording::LocalDirStore;
use tower::ServiceExt as _;

use super::*;
use crate::oidc::OidcVerifier;
use crate::router;
use crate::sessions::SessionStore;

fn state(dir: &std::path::Path) -> Arc<AppState> {
    Arc::new(AppState {
        verifier: OidcVerifier::new("https://issuer.example", "client"),
        sessions: SessionStore::new(Some(b"test-secret"), Duration::from_secs(3600)),
        required_group: None,
        remote_server_artifacts: None,
        transcriber: None,
        vad: None,
        ai_settings: Default::default(),
        make_provider: crate::agent::default_provider_factory(),
        codebase_index: None,
        recordings: Some(Arc::new(LocalDirStore::new(dir.to_owned()))),
    })
}

async fn id_token(state: &AppState, subject: &str) -> String {
    let custom = state
        .sessions
        .sign_in("https://issuer.example", subject, None, None);
    state
        .sessions
        .redeem_custom_token(&custom)
        .await
        .unwrap()
        .id_token
}

async fn call(
    state: &Arc<AppState>,
    method: Method,
    uri: &str,
    token: Option<&str>,
    extra_header: Option<(&str, &str)>,
    body: Body,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if let Some((name, value)) = extra_header {
        request = request.header(name, value);
    }
    let response = router(state.clone())
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, headers, body)
}

fn json_body(value: Value) -> Body {
    Body::from(value.to_string())
}

async fn json_call(
    state: &Arc<AppState>,
    method: Method,
    uri: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let (status, _, bytes) = call(
        state,
        method,
        uri,
        Some(token),
        Some(("content-type", "application/json")),
        json_body(body),
    )
    .await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn start_body() -> Value {
    json!({
        "header": { "version": 2, "width": 80, "height": 24, "timestamp": 1_700_000_000, "title": "zsh" },
        "name": "zsh",
    })
}

#[tokio::test]
async fn round_trips_a_recording() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let token = id_token(&state, "alice").await;

    let (status, created) =
        json_call(&state, Method::POST, "/recordings", &token, start_body()).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap().to_owned();

    for (seq, events) in [
        (0, "[0.5,\"o\",\"hello \"]\n"),
        (1, "[1.5,\"o\",\"world\"]\n"),
    ] {
        let (status, ..) = call(
            &state,
            Method::POST,
            &format!("/recordings/{id}/events?seq={seq}"),
            Some(&token),
            None,
            Body::from(events),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let (status, _, body) = call(
        &state,
        Method::POST,
        &format!("/recordings/{id}/events?seq=5"),
        Some(&token),
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["expected_seq"],
        2
    );

    let (status, _, body) = call(
        &state,
        Method::PATCH,
        &format!("/recordings/{id}"),
        Some(&token),
        Some(("content-type", "application/json")),
        json_body(json!({ "name": "early" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(String::from_utf8(body).unwrap().contains("in progress"));

    let (status, finished) = json_call(
        &state,
        Method::POST,
        &format!("/recordings/{id}/finish"),
        &token,
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(finished["duration"], 1.5);
    assert_eq!(finished["title"], "zsh");
    assert!(finished["local_path"].is_string());

    let (status, list) = json_call(&state, Method::GET, "/recordings", &token, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list, json!([finished]));

    let (status, headers, content) = call(
        &state,
        Method::GET,
        &format!("/recordings/{id}"),
        Some(&token),
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::ACCEPT_RANGES], "bytes");
    let content = String::from_utf8(content).unwrap();
    assert!(content.ends_with("[1.5,\"o\",\"world\"]\n"), "{content}");

    let (status, headers, part) = call(
        &state,
        Method::GET,
        &format!("/recordings/{id}"),
        Some(&token),
        Some(("range", "bytes=0-9")),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(part, content.as_bytes()[..10]);
    assert_eq!(
        headers[header::CONTENT_RANGE],
        format!("bytes 0-9/{}", content.len())
    );

    let (status, renamed) = json_call(
        &state,
        Method::PATCH,
        &format!("/recordings/{id}"),
        &token,
        json!({ "name": "demo" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["id"], "demo");

    let (status, ..) = call(
        &state,
        Method::DELETE,
        "/recordings/demo",
        Some(&token),
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, list) = json_call(&state, Method::GET, "/recordings", &token, Value::Null).await;
    assert_eq!(list, json!([]));
}

#[tokio::test]
async fn requires_sign_in() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    for (method, uri) in [
        (Method::GET, "/recordings"),
        (Method::POST, "/recordings/x/finish"),
        (Method::DELETE, "/recordings/x"),
    ] {
        let (status, ..) = call(&state, method.clone(), uri, None, None, Body::empty()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        let (status, ..) = call(&state, method, uri, Some("bogus"), None, Body::empty()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn keeps_users_apart() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let alice = id_token(&state, "alice").await;
    let bob = id_token(&state, "bob").await;
    let (_, created) = json_call(&state, Method::POST, "/recordings", &alice, start_body()).await;
    let id = created["id"].as_str().unwrap();

    let (_, list) = json_call(&state, Method::GET, "/recordings", &bob, Value::Null).await;
    assert_eq!(list, json!([]));
    for (method, uri) in [
        (Method::GET, format!("/recordings/{id}")),
        (Method::DELETE, format!("/recordings/{id}")),
        (Method::POST, format!("/recordings/{id}/events?seq=0")),
    ] {
        let (status, ..) = call(&state, method, &uri, Some(&bob), None, Body::empty()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    let (_, list) = json_call(&state, Method::GET, "/recordings", &alice, Value::Null).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn reports_missing_storage() {
    let state = Arc::new(AppState {
        recordings: None,
        ..Arc::into_inner(state(std::path::Path::new("/nonexistent"))).unwrap()
    });
    let token = id_token(&state, "alice").await;
    let (status, ..) = call(
        &state,
        Method::GET,
        "/recordings",
        Some(&token),
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
}
