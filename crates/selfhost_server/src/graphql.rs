//! The GraphQL operations the server answers. Each response mirrors the client's `cynic` query
//! types in `crates/graphql`; tests deserialize them through those types.

use serde_json::{Value, json};

use crate::sessions::User;

/// A model the server can run with the user's own credentials. The client shows each one only
/// when the user has connected a credential for its provider.
struct CatalogModel {
    id: &'static str,
    display_name: &'static str,
    description: &'static str,
    /// The provider, as the GraphQL `LlmProvider` enum value.
    provider: &'static str,
    vision: bool,
    context_window: u32,
}

/// Claude only for now: the agent endpoint calls Anthropic. Other providers join when it can call
/// them.
const CATALOG: &[CatalogModel] = &[
    CatalogModel {
        id: "claude-opus-5-5",
        display_name: "Claude Opus 5.5",
        description: "Anthropic's most capable Opus model",
        provider: "ANTHROPIC",
        vision: true,
        context_window: 1_000_000,
    },
    CatalogModel {
        id: "claude-sonnet-5-5",
        display_name: "Claude Sonnet 5.5",
        description: "Fast and capable for everyday coding",
        provider: "ANTHROPIC",
        vision: true,
        context_window: 1_000_000,
    },
    CatalogModel {
        id: "claude-haiku-5-5",
        display_name: "Claude Haiku 5.5",
        description: "Fastest and cheapest Claude model",
        provider: "ANTHROPIC",
        vision: true,
        context_window: 1_000_000,
    },
    CatalogModel {
        id: "claude-fable-5-2",
        display_name: "Claude Fable 5.2",
        description: "Anthropic's most capable model, for the hardest tasks",
        provider: "ANTHROPIC",
        vision: true,
        context_window: 1_000_000,
    },
];

const DEFAULT_MODEL_ID: &str = "claude-opus-5-5";

pub fn get_user_response(user: &User) -> Value {
    json!({
        "data": {
            "user": {
                "__typename": "UserOutput",
                "apiKeyOwnerType": null,
                "principalType": "USER",
                "user": {
                    "anonymousUserInfo": null,
                    "experiments": [],
                    "globalSkills": [],
                    "isOnboarded": true,
                    "isOnWorkDomain": true,
                    "profile": {
                        "displayName": user.display_name,
                        "email": user.email,
                        "needsSsoLink": false,
                        "photoUrl": null,
                        "uid": user.uid,
                    },
                    "llms": {
                        "agentMode": available_models(),
                        "planning": available_models(),
                        "coding": available_models(),
                        "cliAgent": available_models(),
                        "computerUseAgent": available_models(),
                    },
                },
            },
        },
    })
}

fn available_models() -> Value {
    json!({
        "defaultId": DEFAULT_MODEL_ID,
        "preferredCodexModelId": null,
        "choices": CATALOG.iter().map(model_json).collect::<Vec<_>>(),
    })
}

fn model_json(model: &CatalogModel) -> Value {
    json!({
        "id": model.id,
        "displayName": model.display_name,
        "baseModelName": model.id,
        "reasoningLevel": null,
        "description": model.description,
        "disableReason": null,
        "visionSupported": model.vision,
        "spec": null,
        "provider": model.provider,
        "hostConfigs": [],
        "usageMetadata": { "creditMultiplier": null, "requestMultiplier": 1 },
        "pricing": { "discountPercentage": null },
        "contextWindow": {
            "isConfigurable": false,
            "min": model.context_window,
            "max": model.context_window,
            "default": model.context_window,
        },
    })
}

/// The response for operations the server doesn't implement yet.
pub fn unsupported_operation_response(operation: &str) -> Value {
    json!({
        "data": null,
        "errors": [{
            "message": format!("{operation} is not supported by this self-hosted server yet"),
        }],
    })
}
