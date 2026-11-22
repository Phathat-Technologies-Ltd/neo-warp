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

fn get_user_response(user: &User) -> Value {
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

/// An operation the server answers for a signed-in user.
#[derive(Clone, Copy)]
pub enum Operation {
    GetUser,
    GetUserSettings,
    GetRequestLimitInfo,
    GetWorkspacesMetadataForUser,
    GetUpdatedCloudObjects,
    ListAIConversationMetadata,
    GetAvailableHarnesses,
    GetChatGPTConnection,
    TuiOnboardingMarkers,
    GetAICreditAvailability,
    GetDiscoverableTeams,
    GetConversationUsage,
    GetCloudEnvironmentsQuery,
}

impl Operation {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "GetUser" => Self::GetUser,
            "GetUserSettings" => Self::GetUserSettings,
            "GetRequestLimitInfo" => Self::GetRequestLimitInfo,
            "GetWorkspacesMetadataForUser" => Self::GetWorkspacesMetadataForUser,
            "GetUpdatedCloudObjects" => Self::GetUpdatedCloudObjects,
            "ListAIConversationMetadata" => Self::ListAIConversationMetadata,
            "GetAvailableHarnesses" => Self::GetAvailableHarnesses,
            "GetChatGPTConnection" => Self::GetChatGPTConnection,
            "TuiOnboardingMarkers" => Self::TuiOnboardingMarkers,
            "GetAICreditAvailability" => Self::GetAICreditAvailability,
            "GetDiscoverableTeams" => Self::GetDiscoverableTeams,
            "GetConversationUsage" => Self::GetConversationUsage,
            "GetCloudEnvironmentsQuery" => Self::GetCloudEnvironmentsQuery,
            _ => return None,
        })
    }

    pub fn response(self, user: &User) -> Value {
        match self {
            Self::GetUser => get_user_response(user),
            Self::GetUserSettings => get_user_settings_response(),
            Self::GetRequestLimitInfo => get_request_limit_info_response(),
            Self::GetWorkspacesMetadataForUser => get_workspaces_metadata_response(user),
            Self::GetUpdatedCloudObjects => get_updated_cloud_objects_response(),
            Self::ListAIConversationMetadata => list_ai_conversation_metadata_response(),
            Self::GetAvailableHarnesses => get_available_harnesses_response(),
            Self::GetChatGPTConnection => get_chatgpt_connection_response(),
            Self::TuiOnboardingMarkers => tui_onboarding_markers_response(),
            Self::GetAICreditAvailability => user_output(json!({
                "aiCreditAvailability": ai_credit_availability(),
            })),
            Self::GetDiscoverableTeams => user_output(json!({
                "discoverableTeams": [],
                "discoverableWorkspaces": [],
            })),
            // Usage history lives with the model provider, not this server.
            Self::GetConversationUsage => user_output(json!({ "conversationUsage": [] })),
            Self::GetCloudEnvironmentsQuery => json!({
                "data": {
                    "getCloudEnvironments": {
                        "__typename": "GetCloudEnvironmentsOutput",
                        "cloudEnvironments": [],
                        "responseContext": { "serverVersion": null },
                    },
                },
            }),
        }
    }
}

/// A `user` query answer whose user carries `fields`.
fn user_output(fields: Value) -> Value {
    json!({
        "data": {
            "user": {
                "__typename": "UserOutput",
                "user": fields,
            },
        },
    })
}

/// Organization users bring their own credentials, so AI is never out of credits.
fn ai_credit_availability() -> Value {
    json!({
        "available": true,
        "denialReason": "NONE",
        "creditSource": null,
    })
}

/// A null `settings` leaves the client's local settings in charge.
fn get_user_settings_response() -> Value {
    json!({
        "data": {
            "user": {
                "__typename": "UserOutput",
                "user": { "settings": null },
            },
        },
    })
}

/// Organization users bring their own API keys, so the server never limits requests. The refresh
/// time is far off so the client never treats the quota as due for a reset.
fn get_request_limit_info_response() -> Value {
    json!({
        "data": {
            "user": {
                "__typename": "UserOutput",
                "user": {
                    "workspaces": [],
                    "bonusGrants": [],
                    "requestLimitInfo": {
                        "isUnlimited": true,
                        "nextRefreshTime": "2100-01-01T00:00:00Z",
                        "requestLimit": UNLIMITED_REQUESTS,
                        "requestsUsedSinceLastRefresh": 0,
                        "includedUsageCents": null,
                        "usageCentsUsedSinceLastRefresh": null,
                        "requestLimitRefreshDuration": "MONTHLY",
                        "isUnlimitedVoice": true,
                        "voiceRequestLimit": UNLIMITED_REQUESTS,
                        "voiceRequestsUsedSinceLastRefresh": 0,
                        "isUnlimitedCodebaseIndices": true,
                        "maxCodebaseIndices": 40,
                        "maxFilesPerRepo": 10000,
                        "embeddingGenerationBatchSize": 100,
                    },
                },
            },
        },
    })
}

/// The client reports the limit as the remaining requests even when `isUnlimited` is set, so it
/// must stay large.
const UNLIMITED_REQUESTS: i32 = 999_999;

fn get_workspaces_metadata_response(user: &User) -> Value {
    json!({
        "data": {
            "user": {
                "__typename": "UserOutput",
                "user": {
                    "profile": { "uid": user.uid },
                    "aiCreditAvailability": ai_credit_availability(),
                    "billingMetadata": null,
                    "workspaces": [],
                    "experiments": [],
                    "discoverableTeams": [],
                },
            },
            "pricingInfo": {
                "__typename": "PricingInfoOutput",
                "pricingInfo": {
                    "plans": [],
                    "overages": { "pricePerRequestUsdCents": 0 },
                    "addonCreditsOptions": [],
                    "promotionMessage": null,
                },
            },
        },
    })
}

fn get_updated_cloud_objects_response() -> Value {
    json!({
        "data": {
            "updatedCloudObjects": {
                "__typename": "UpdatedCloudObjectsOutput",
                "actionHistories": [],
                "deletedObjectUids": {
                    "folderUids": [],
                    "genericStringObjectUids": [],
                    "notebookUids": [],
                    "workflowUids": [],
                },
                "folders": [],
                "genericStringObjects": [],
                "mcpGallery": [],
                "notebooks": [],
                "responseContext": { "serverVersion": null },
                "userProfiles": [],
                "workflows": [],
            },
        },
    })
}

fn list_ai_conversation_metadata_response() -> Value {
    json!({
        "data": {
            "listAIConversations": {
                "__typename": "ListAIConversationsOutput",
                "conversations": [],
                "responseContext": { "serverVersion": null },
            },
        },
    })
}

/// The client's own default when it has never heard from a server: only the Warp agent, enabled.
fn get_available_harnesses_response() -> Value {
    json!({
        "data": {
            "user": {
                "__typename": "UserOutput",
                "user": {
                    "availableHarnesses": {
                        "harnesses": [{
                            "harness": "OZ",
                            "displayName": "Warp Agent",
                            "enabled": true,
                            "availableModels": [],
                        }],
                    },
                },
            },
        },
    })
}

fn get_chatgpt_connection_response() -> Value {
    json!({
        "data": {
            "user": {
                "__typename": "UserOutput",
                "user": { "chatgptConnection": null },
            },
        },
    })
}

/// Both markers read as already shown so the TUI skips its first-run zero state and credit gate.
fn tui_onboarding_markers_response() -> Value {
    json!({
        "data": {
            "tuiOnboardingMarkers": {
                "__typename": "TuiOnboardingMarkersOutput",
                "firstZeroStateShown": true,
                "firstCreditGateShown": true,
            },
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
