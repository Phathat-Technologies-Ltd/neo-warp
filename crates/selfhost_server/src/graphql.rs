//! The GraphQL operations the server answers. Each response mirrors the client's `cynic` query
//! types in `crates/graphql`; tests deserialize them through those types.

use serde_json::{Value, json};

use crate::sessions::User;

/// The model shown until the server serves a real catalog. The client needs at least one model
/// per feature.
const PLACEHOLDER_MODEL_ID: &str = "selfhost-no-models";

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
                        "agentMode": placeholder_models(),
                        "planning": placeholder_models(),
                        "coding": placeholder_models(),
                        "cliAgent": placeholder_models(),
                        "computerUseAgent": placeholder_models(),
                    },
                },
            },
        },
    })
}

fn placeholder_models() -> Value {
    json!({
        "defaultId": PLACEHOLDER_MODEL_ID,
        "preferredCodexModelId": null,
        "choices": [{
            "id": PLACEHOLDER_MODEL_ID,
            "displayName": "No models configured",
            "baseModelName": PLACEHOLDER_MODEL_ID,
            "reasoningLevel": null,
            "description": "This self-hosted server doesn't serve models yet.",
            "disableReason": null,
            "visionSupported": false,
            "spec": null,
            "provider": "UNKNOWN",
            "hostConfigs": [],
            "usageMetadata": { "creditMultiplier": null, "requestMultiplier": 1 },
            "pricing": { "discountPercentage": null },
            "contextWindow": {
                "isConfigurable": false,
                "min": 0,
                "max": 0,
                "default": 0,
            },
        }],
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
