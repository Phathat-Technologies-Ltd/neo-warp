use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::{Body, to_bytes};
use axum::http::Request;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{EncodingKey, Header};
use serde_json::Value;
use tower::ServiceExt as _;

use super::*;

const ISSUER: &str = "https://login.example/tenant/v2.0";
const CLIENT_ID: &str = "client-123";
/// Test-only RSA key standing in for the identity provider's signing key.
const PROVIDER_KEY_PEM: &str = include_str!("testdata/test_only_rsa_key.pem");
const PROVIDER_JWKS: &str = include_str!("testdata/test_only_jwks.json");

fn state_with(required_group: Option<&str>, max_age: Duration) -> Arc<AppState> {
    let keys: JwkSet = serde_json::from_str(PROVIDER_JWKS).unwrap();
    Arc::new(AppState {
        verifier: OidcVerifier::with_keys(ISSUER, CLIENT_ID, keys),
        sessions: SessionStore::new(Some(b"test-secret"), max_age),
        required_group: required_group.map(str::to_owned),
        remote_server_artifacts: None,
        transcriber: None,
        ai_settings: Default::default(),
        make_provider: crate::agent::default_provider_factory(),
    })
}

fn state() -> Arc<AppState> {
    state_with(None, Duration::from_secs(12 * 60 * 60))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn provider_claims() -> Value {
    json!({
        "iss": ISSUER,
        "aud": CLIENT_ID,
        "sub": "provider-subject",
        "email": "ada@example.com",
        "name": "Ada Lovelace",
        "iat": now(),
        "exp": now() + 3600,
    })
}

fn provider_token(claims: &Value, kid: &str) -> String {
    let mut header = Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(kid.to_owned());
    jsonwebtoken::encode(
        &header,
        claims,
        &EncodingKey::from_rsa_pem(PROVIDER_KEY_PEM.as_bytes()).unwrap(),
    )
    .unwrap()
}

async fn send(state: &Arc<AppState>, request: Request<Body>) -> (StatusCode, Value) {
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

fn json_post(path: &str, body: Value) -> Request<Body> {
    Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn form_post(path: &str, fields: &[(&str, &str)]) -> Request<Body> {
    Request::post(path)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(serde_urlencoded_string(fields)))
        .unwrap()
}

fn serde_urlencoded_string(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(key, value)| format!("{key}={}", value.replace('+', "%2B").replace('/', "%2F")))
        .collect::<Vec<_>>()
        .join("&")
}

fn graphql_post(operation: &str, id_token: Option<&str>) -> Request<Body> {
    let mut request = Request::post(format!("/graphql/v2?op={operation}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(id_token) = id_token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {id_token}"));
    }
    request
        .body(Body::from(
            json!({ "query": "", "variables": {} }).to_string(),
        ))
        .unwrap()
}

async fn exchange(state: &Arc<AppState>, claims: &Value) -> (StatusCode, Value) {
    send(
        state,
        json_post(
            "/auth/sso/exchange",
            json!({ "id_token": provider_token(claims, "test-key") }),
        ),
    )
    .await
}

async fn sign_in(state: &Arc<AppState>) -> (String, String) {
    let (status, body) = exchange(state, &provider_claims()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let custom_token = body["custom_token"].as_str().unwrap();
    let (status, body) = send(
        state,
        form_post(
            "/v1/accounts:signInWithCustomToken",
            &[("returnSecureToken", "true"), ("token", custom_token)],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["expiresIn"], "3600");
    (
        body["idToken"].as_str().unwrap().to_owned(),
        body["refreshToken"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test]
async fn sign_in_then_get_user_returns_the_provider_identity() {
    let state = state();
    let (id_token, _) = sign_in(&state).await;

    let (status, body) = send(&state, graphql_post("GetUser", Some(&id_token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let response: cynic::GraphQlResponse<warp_graphql::queries::get_user::GetUser> =
        serde_json::from_value(body).expect("GetUser response must match the client's query");
    let warp_graphql::queries::get_user::UserResult::UserOutput(output) =
        response.data.expect("GetUser should return data").user
    else {
        panic!("GetUser should return a UserOutput");
    };
    assert_eq!(
        output.user.profile.email.as_deref(),
        Some("ada@example.com")
    );
    assert_eq!(
        output.user.profile.display_name.as_deref(),
        Some("Ada Lovelace")
    );
    assert!(!output.user.llms.agent_mode.choices.is_empty());
    assert_eq!(
        output.user.llms.agent_mode.default_id,
        output.user.llms.agent_mode.choices[0].id
    );
}

#[tokio::test]
async fn user_id_is_stable_for_the_same_provider_identity() {
    let state = state();
    let uid = |body: Value| body["data"]["user"]["user"]["profile"]["uid"].clone();
    let (first, _) = sign_in(&state).await;
    let (second, _) = sign_in(&state).await;
    let (_, first_user) = send(&state, graphql_post("GetUser", Some(&first))).await;
    let (_, second_user) = send(&state, graphql_post("GetUser", Some(&second))).await;
    assert_eq!(uid(first_user), uid(second_user));
}

fn refresh_request(token: &str) -> Request<Body> {
    form_post(
        "/v1/token",
        &[("grant_type", "refresh_token"), ("refresh_token", token)],
    )
}

#[tokio::test]
async fn refresh_issues_fresh_working_tokens() {
    let state = state();
    let (_, refresh_token) = sign_in(&state).await;

    let (status, body) = send(&state, refresh_request(&refresh_token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["expires_in"], "3600");
    assert_ne!(body["refresh_token"].as_str().unwrap(), refresh_token);
    let (status, _) = send(&state, graphql_post("GetUser", body["id_token"].as_str())).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn refresh_fails_once_the_sign_in_is_older_than_the_max_age() {
    let state = state_with(None, Duration::ZERO);
    let (_, refresh_token) = sign_in(&state).await;
    // Token expiry has one-second resolution.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (status, body) = send(&state, refresh_request(&refresh_token)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], "INVALID_REFRESH_TOKEN");
}

#[tokio::test]
async fn sign_ins_survive_a_restart_with_the_same_secret() {
    let (id_token, refresh_token) = sign_in(&state()).await;
    let restarted = state();
    assert_eq!(
        send(&restarted, graphql_post("GetUser", Some(&id_token)))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        send(&restarted, refresh_request(&refresh_token)).await.0,
        StatusCode::OK
    );

    let other_secret = Arc::new(AppState {
        sessions: SessionStore::new(Some(b"other-secret"), Duration::from_secs(3600)),
        ..Arc::into_inner(state()).unwrap()
    });
    assert_eq!(
        send(&other_secret, graphql_post("GetUser", Some(&id_token)))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn tokens_only_work_for_their_own_purpose() {
    let state = state();
    let (id_token, refresh_token) = sign_in(&state).await;
    assert_eq!(
        send(&state, graphql_post("GetUser", Some(&refresh_token)))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, body) = send(&state, refresh_request(&id_token)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], "INVALID_REFRESH_TOKEN");
    let (status, body) = send(
        &state,
        form_post(
            "/v1/accounts:signInWithCustomToken",
            &[("token", &id_token)],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], "INVALID_CUSTOM_TOKEN");
}

#[tokio::test]
async fn custom_tokens_are_single_use() {
    let state = state();
    let (_, body) = exchange(&state, &provider_claims()).await;
    let custom_token = body["custom_token"].as_str().unwrap().to_owned();
    let redeem = || {
        form_post(
            "/v1/accounts:signInWithCustomToken",
            &[("token", &custom_token)],
        )
    };
    assert_eq!(send(&state, redeem()).await.0, StatusCode::OK);
    let (status, body) = send(&state, redeem()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], "INVALID_CUSTOM_TOKEN");
}

#[tokio::test]
async fn provider_tokens_for_another_client_issuer_or_key_are_rejected() {
    let state = state();
    let mut wrong_audience = provider_claims();
    wrong_audience["aud"] = json!("someone-else");
    let mut wrong_issuer = provider_claims();
    wrong_issuer["iss"] = json!("https://evil.example");
    let mut expired = provider_claims();
    expired["exp"] = json!(now() - 3600);
    for claims in [wrong_audience, wrong_issuer, expired] {
        assert_eq!(exchange(&state, &claims).await.0, StatusCode::UNAUTHORIZED);
    }

    let unknown_key = provider_token(&provider_claims(), "unknown-key");
    let (status, _) = send(
        &state,
        json_post("/auth/sso/exchange", json!({ "id_token": unknown_key })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = send(
        &state,
        json_post("/auth/sso/exchange", json!({ "id_token": "not-a-jwt" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn required_group_is_enforced() {
    let state = state_with(Some("warp-users"), Duration::from_secs(3600));
    assert_eq!(
        exchange(&state, &provider_claims()).await.0,
        StatusCode::FORBIDDEN
    );
    let mut member = provider_claims();
    member["groups"] = json!(["other", "warp-users"]);
    assert_eq!(exchange(&state, &member).await.0, StatusCode::OK);
}

/// Asks for `operation` as a signed-in user and parses the body through the client's query type.
async fn signed_in_query<T: serde::de::DeserializeOwned>(operation: &str) -> T {
    let state = state();
    let (id_token, _) = sign_in(&state).await;
    let (status, body) = send(&state, graphql_post(operation, Some(&id_token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response: cynic::GraphQlResponse<T> = serde_json::from_value(body)
        .unwrap_or_else(|e| panic!("{operation} response must match the client's query: {e}"));
    assert!(response.errors.is_none(), "{operation} returned errors");
    response
        .data
        .unwrap_or_else(|| panic!("{operation} should return data"))
}

#[tokio::test]
async fn get_user_settings_stores_nothing() {
    use warp_graphql::queries::get_user_settings::{GetUserSettings, UserResult};
    let data: GetUserSettings = signed_in_query("GetUserSettings").await;
    let UserResult::UserOutput(output) = data.user else {
        panic!("GetUserSettings should return a UserOutput");
    };
    assert!(output.user.settings.is_none());
}

#[tokio::test]
async fn request_limits_never_block_the_user() {
    use warp_graphql::queries::get_request_limit_info::{GetRequestLimitInfo, UserResult};
    let data: GetRequestLimitInfo = signed_in_query("GetRequestLimitInfo").await;
    let UserResult::UserOutput(output) = data.user else {
        panic!("GetRequestLimitInfo should return a UserOutput");
    };
    let limits = output.user.request_limit_info;
    assert!(limits.is_unlimited);
    assert!(limits.request_limit > 0);
    assert_eq!(limits.requests_used_since_last_refresh, 0);
    assert!(output.user.workspaces.is_empty());
    assert!(output.user.bonus_grants.is_empty());
}

#[tokio::test]
async fn workspaces_metadata_has_no_workspaces_and_available_credits() {
    use warp_graphql::queries::get_workspaces_metadata_for_user::{
        GetWorkspacesMetadataForUser, PricingInfoResult, UserResult,
    };
    let data: GetWorkspacesMetadataForUser = signed_in_query("GetWorkspacesMetadataForUser").await;
    let UserResult::UserOutput(output) = data.user else {
        panic!("GetWorkspacesMetadataForUser should return a UserOutput");
    };
    assert!(output.user.workspaces.is_empty());
    assert!(output.user.discoverable_teams.is_empty());
    assert!(output.user.ai_credit_availability.available);
    assert!(!output.user.profile.uid.is_empty());
    assert!(matches!(
        data.pricing_info,
        PricingInfoResult::PricingInfoOutput(_)
    ));
}

#[tokio::test]
async fn updated_cloud_objects_are_empty() {
    use warp_graphql::queries::get_updated_cloud_objects::{
        GetUpdatedCloudObjects, UpdatedCloudObjectsResult,
    };
    let data: GetUpdatedCloudObjects = signed_in_query("GetUpdatedCloudObjects").await;
    let UpdatedCloudObjectsResult::UpdatedCloudObjectsOutput(output) = data.updated_cloud_objects
    else {
        panic!("GetUpdatedCloudObjects should return an UpdatedCloudObjectsOutput");
    };
    assert!(output.notebooks.unwrap().is_empty());
    assert!(output.workflows.unwrap().is_empty());
}

#[tokio::test]
async fn ai_conversation_metadata_is_empty() {
    use warp_graphql::queries::list_ai_conversations::{
        ListAIConversationMetadata, ListAIConversationMetadataResult,
    };
    let data: ListAIConversationMetadata = signed_in_query("ListAIConversationMetadata").await;
    let ListAIConversationMetadataResult::ListAIConversationsOutput(output) =
        data.list_ai_conversations
    else {
        panic!("ListAIConversationMetadata should return a ListAIConversationsOutput");
    };
    assert!(output.conversations.is_empty());
}

#[tokio::test]
async fn only_the_warp_agent_harness_is_available() {
    use warp_graphql::ai::AgentHarness;
    use warp_graphql::queries::get_available_harnesses::{GetAvailableHarnesses, UserResult};
    let data: GetAvailableHarnesses = signed_in_query("GetAvailableHarnesses").await;
    let UserResult::UserOutput(output) = data.user else {
        panic!("GetAvailableHarnesses should return a UserOutput");
    };
    let harnesses = output.user.available_harnesses.harnesses;
    assert_eq!(harnesses.len(), 1);
    assert!(matches!(harnesses[0].harness, AgentHarness::Oz));
    assert!(harnesses[0].enabled);
}

#[tokio::test]
async fn chatgpt_is_not_connected() {
    use warp_graphql::queries::get_chatgpt_connection::{GetChatGPTConnection, UserResult};
    let data: GetChatGPTConnection = signed_in_query("GetChatGPTConnection").await;
    let UserResult::UserOutput(output) = data.user else {
        panic!("GetChatGPTConnection should return a UserOutput");
    };
    assert!(output.user.chatgpt_connection.is_none());
}

#[tokio::test]
async fn tui_onboarding_markers_read_as_already_shown() {
    use warp_graphql::queries::tui_onboarding_markers::{
        TuiOnboardingMarkers, TuiOnboardingMarkersResult,
    };
    let data: TuiOnboardingMarkers = signed_in_query("TuiOnboardingMarkers").await;
    let TuiOnboardingMarkersResult::TuiOnboardingMarkersOutput(markers) =
        data.tui_onboarding_markers
    else {
        panic!("TuiOnboardingMarkers should return a TuiOnboardingMarkersOutput");
    };
    assert!(markers.first_zero_state_shown);
    assert!(markers.first_credit_gate_shown);
}

#[tokio::test]
async fn ai_credits_are_always_available() {
    use warp_graphql::queries::get_ai_credit_availability::{GetAICreditAvailability, UserResult};
    let data: GetAICreditAvailability = signed_in_query("GetAICreditAvailability").await;
    let UserResult::UserOutput(output) = data.user else {
        panic!("GetAICreditAvailability should return a UserOutput");
    };
    assert!(output.user.ai_credit_availability.available);
}

#[tokio::test]
async fn there_are_no_teams_to_discover() {
    use warp_graphql::queries::get_discoverable_teams::{GetDiscoverableTeams, UserResult};
    let data: GetDiscoverableTeams = signed_in_query("GetDiscoverableTeams").await;
    let UserResult::UserOutput(output) = data.user else {
        panic!("GetDiscoverableTeams should return a UserOutput");
    };
    assert!(output.user.discoverable_teams.is_empty());
    assert!(output.user.discoverable_workspaces.is_empty());
}

#[tokio::test]
async fn conversation_usage_is_empty() {
    use warp_graphql::queries::get_conversation_usage::{GetConversationUsage, UserResult};
    let data: GetConversationUsage = signed_in_query("GetConversationUsage").await;
    let UserResult::UserOutput(output) = data.user else {
        panic!("GetConversationUsage should return a UserOutput");
    };
    assert!(output.user.conversation_usage.is_empty());
}

#[tokio::test]
async fn there_are_no_cloud_environments() {
    use warp_graphql::queries::get_cloud_environments::{
        GetCloudEnvironmentsQuery, GetCloudEnvironmentsResult,
    };
    let data: GetCloudEnvironmentsQuery = signed_in_query("GetCloudEnvironmentsQuery").await;
    let GetCloudEnvironmentsResult::GetCloudEnvironmentsOutput(output) =
        data.get_cloud_environments
    else {
        panic!("GetCloudEnvironmentsQuery should return a GetCloudEnvironmentsOutput");
    };
    assert!(output.cloud_environments.is_empty());
}

#[tokio::test]
async fn signed_in_operations_require_a_valid_server_token() {
    let state = state();
    assert_eq!(
        send(&state, graphql_post("GetRequestLimitInfo", None))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn get_user_requires_a_valid_server_token() {
    let state = state();
    assert_eq!(
        send(&state, graphql_post("GetUser", None)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let provider_token = provider_token(&provider_claims(), "test-key");
    assert_eq!(
        send(&state, graphql_post("GetUser", Some(&provider_token)))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn unsupported_operations_return_graphql_errors() {
    let state = state();
    let (id_token, _) = sign_in(&state).await;
    let (status, body) = send(
        &state,
        graphql_post("GetCloudEnvironments", Some(&id_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["data"].is_null());
    assert!(
        body["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("GetCloudEnvironments")
    );
}

#[test]
fn email_falls_back_to_the_preferred_username() {
    let claims: oidc::ProviderClaims = serde_json::from_value(json!({
        "iss": ISSUER,
        "sub": "s",
        "preferred_username": "ada@example.com",
    }))
    .unwrap();
    assert_eq!(claims.email(), Some("ada@example.com"));
}

#[test]
fn embedded_server_answers_on_its_reported_port() {
    let address = spawn_embedded(EmbeddedConfig {
        issuer: ISSUER.to_owned(),
        client_id: CLIENT_ID.to_owned(),
        signing_secret: b"test-secret".to_vec(),
        session_max_age: Duration::from_secs(3600),
        remote_server_artifacts: None,
        transcriber: None,
    })
    .expect("embedded server should start");
    assert!(address.ip().is_loopback());
    let status = tokio::runtime::Runtime::new().unwrap().block_on(async {
        reqwest::Client::new()
            .post(format!("http://{address}/graphql/v2?op=GetUser"))
            .send()
            .await
            .expect("embedded server should answer")
            .status()
    });
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn model_catalog_lists_claude_models_with_opus_as_the_default() {
    let state = state();
    let (id_token, _) = sign_in(&state).await;
    let (_, body) = send(&state, graphql_post("GetUser", Some(&id_token))).await;
    let llms = &body["data"]["user"]["user"]["llms"];
    for feature in [
        "agentMode",
        "planning",
        "coding",
        "cliAgent",
        "computerUseAgent",
    ] {
        let models = &llms[feature];
        assert_eq!(models["defaultId"], "claude-opus-5-5", "{feature}");
        let choices = models["choices"].as_array().unwrap();
        assert!(choices.iter().all(|model| model["provider"] == "ANTHROPIC"));
        assert!(
            choices
                .iter()
                .any(|model| model["id"] == "claude-sonnet-5-5")
        );
    }
}

fn download_request(os: &str, arch: &str) -> Request<Body> {
    Request::get(format!(
        "/download/cli?package=tar&os={os}&arch={arch}&channel=dev"
    ))
    .body(Body::empty())
    .unwrap()
}

#[tokio::test]
async fn serves_remote_server_builds_for_linux() {
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::write(
        artifacts.path().join("remote-server-linux-aarch64.tar.gz"),
        b"arm64 build",
    )
    .unwrap();
    let state = Arc::new(AppState {
        remote_server_artifacts: Some(artifacts.path().to_owned()),
        ..Arc::into_inner(state()).unwrap()
    });

    let (status, body) = send(&state, download_request("linux", "aarch64")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, Value::String("arm64 build".to_owned()));

    // Missing builds and unknown platforms, including path-like values, are not found.
    for (os, arch) in [
        ("linux", "x86_64"),
        ("darwin", "aarch64"),
        ("linux", "..%2F..%2Fetc"),
    ] {
        assert_eq!(
            send(&state, download_request(os, arch)).await.0,
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn remote_server_downloads_need_configured_builds() {
    assert_eq!(
        send(&state(), download_request("linux", "x86_64")).await.0,
        StatusCode::NOT_FOUND
    );
}

fn generate_commands_post(id_token: &str, prompt: &str) -> Request<Body> {
    Request::post("/graphql/v2?op=GenerateCommands")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {id_token}"))
        .body(Body::from(
            json!({
                "query": "",
                "variables": {
                    "input": { "prompt": prompt },
                    "requestContext": {
                        "clientContext": { "version": null },
                        "osContext": { "category": "macOS", "name": "macOS", "version": "15.0" },
                    },
                },
            })
            .to_string(),
        ))
        .unwrap()
}

fn parse_generate_commands(
    body: Value,
) -> warp_graphql::mutations::generate_commands::GenerateCommandsStatus {
    use warp_graphql::mutations::generate_commands::{GenerateCommands, GenerateCommandsResult};
    let response: cynic::GraphQlResponse<GenerateCommands> = serde_json::from_value(body)
        .expect("GenerateCommands response must match the client's mutation");
    let GenerateCommandsResult::GenerateCommandsOutput(output) =
        response.data.unwrap().generate_commands
    else {
        panic!("GenerateCommands should return a GenerateCommandsOutput");
    };
    output.status
}

#[tokio::test]
async fn command_search_needs_model_settings_from_an_earlier_request() {
    use warp_graphql::mutations::generate_commands::{
        GenerateCommandsFailureType, GenerateCommandsStatus,
    };
    let state = state();
    let (id_token, _) = sign_in(&state).await;
    let (status, body) = send(&state, generate_commands_post(&id_token, "list files")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(matches!(
        parse_generate_commands(body),
        GenerateCommandsStatus::GenerateCommandsFailure(failure)
            if matches!(failure.type_, GenerateCommandsFailureType::AiProviderError)
    ));
}

#[tokio::test]
async fn command_search_uses_the_settings_of_the_latest_suggestion_request() {
    use prost::Message as _;
    use warp_graphql::mutations::generate_commands::GenerateCommandsStatus;
    use warp_multi_agent_api as api;

    let provider = crate::agent::scripted::TextProvider::new(vec![Ok(
        r#"[{"command": "ls -la", "description": "List all files", "parameters": []}]"#,
    )]);
    let state = Arc::new(AppState {
        make_provider: provider.factory(),
        ..Arc::into_inner(state()).unwrap()
    });
    let (id_token, _) = sign_in(&state).await;

    let suggestion_request = api::Request {
        settings: Some(api::request::Settings {
            model_config: Some(api::request::settings::ModelConfig {
                base: "claude-haiku-5-5".to_owned(),
                ..Default::default()
            }),
            api_keys: Some(api::request::settings::ApiKeys {
                anthropic: "sk-ant-key".to_owned(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (status, _) = send(
        &state,
        Request::post("/ai/passive-suggestions")
            .header(header::AUTHORIZATION, format!("Bearer {id_token}"))
            .body(Body::from(suggestion_request.encode_to_vec()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, body) = send(&state, generate_commands_post(&id_token, "list all files")).await;
    let GenerateCommandsStatus::GenerateCommandsSuccess(success) = parse_generate_commands(body)
    else {
        panic!("command search should succeed");
    };
    assert_eq!(success.commands.len(), 1);
    assert_eq!(success.commands[0].command, "ls -la");
    assert!(provider.calls()[0].2.contains("macOS 15.0"));
}

#[tokio::test]
async fn legacy_prompt_suggestions_answer_null_without_settings() {
    let state = state();
    let (id_token, _) = sign_in(&state).await;
    let (status, body) = send(
        &state,
        Request::post("/ai/generate_am_query_suggestions")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {id_token}"))
            .body(Body::from(
                json!({ "context_messages": ["ls"], "exit_code": 0 }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["suggestion"].is_null());
    assert!(body["id"].is_string());
}
