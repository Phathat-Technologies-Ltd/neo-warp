//! In-process evaluation of organization feature flags written in the flagd flag-definition format
//! (<https://flagd.dev/schema/v0/flags.json>).
//!
//! Only the static subset is evaluated: `state`, `variants`, and `defaultVariant`. Flags with
//! `targeting` resolve to their default variant, so the same file can later be served by a real
//! flagd without changes.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Deserialize;

/// The admin-written flag file, which lives next to `organization.toml`.
pub const ADMIN_FLAGS_FILE_NAME: &str = "flags.flagd.json";

/// Names a flag file that overrides the admin file, honored only when overrides are allowed.
pub const FLAGS_FILE_ENV: &str = "WARP_ORG_FLAGS_FILE";

const BUNDLED_DEFAULTS: &str = include_str!("../resources/default.flagd.json");

/// A product area an organization can turn on or off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProductArea {
    WarpDrive,
    SettingsSync,
    CloudAgents,
    WarpHostedMcp,
    SharingAndTeams,
    BillingAndReferrals,
    BlockTitles,
    ConversationCloudSync,
    VoiceInput,
    CodebaseIndexing,
    AiPromptSuggestions,
    AiCommandSearch,
    SessionRecording,
}

impl ProductArea {
    pub const ALL: [ProductArea; 13] = [
        ProductArea::WarpDrive,
        ProductArea::SettingsSync,
        ProductArea::CloudAgents,
        ProductArea::WarpHostedMcp,
        ProductArea::SharingAndTeams,
        ProductArea::BillingAndReferrals,
        ProductArea::BlockTitles,
        ProductArea::ConversationCloudSync,
        ProductArea::VoiceInput,
        ProductArea::CodebaseIndexing,
        ProductArea::AiPromptSuggestions,
        ProductArea::AiCommandSearch,
        ProductArea::SessionRecording,
    ];

    /// The flag key in flag files.
    pub fn key(self) -> &'static str {
        match self {
            ProductArea::WarpDrive => "warp_drive",
            ProductArea::SettingsSync => "settings_sync",
            ProductArea::CloudAgents => "cloud_agents",
            ProductArea::WarpHostedMcp => "warp_hosted_mcp",
            ProductArea::SharingAndTeams => "sharing_and_teams",
            ProductArea::BillingAndReferrals => "billing_and_referrals",
            ProductArea::BlockTitles => "block_titles",
            ProductArea::ConversationCloudSync => "conversation_cloud_sync",
            ProductArea::VoiceInput => "voice_input",
            ProductArea::CodebaseIndexing => "codebase_indexing",
            ProductArea::AiPromptSuggestions => "ai_prompt_suggestions",
            ProductArea::AiCommandSearch => "ai_command_search",
            ProductArea::SessionRecording => "session_recording",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|area| area.key() == key)
    }
}

#[derive(Deserialize)]
struct FlagFile {
    #[serde(default)]
    flags: BTreeMap<String, FlagDefinition>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FlagDefinition {
    state: FlagState,
    #[serde(default)]
    variants: HashMap<String, serde_json::Value>,
    default_variant: Option<String>,
    targeting: Option<serde_json::Value>,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
enum FlagState {
    Enabled,
    Disabled,
}

/// Evaluates a flagd flag file to the boolean value of each enabled flag. Disabled flags and flags
/// that don't resolve to a boolean are left out.
pub fn evaluate(json: &str) -> anyhow::Result<BTreeMap<String, bool>> {
    let file: FlagFile = serde_json::from_str(json).context("invalid flagd flag file")?;
    let mut values = BTreeMap::new();
    for (key, flag) in file.flags {
        if flag.state == FlagState::Disabled {
            continue;
        }
        if flag
            .targeting
            .as_ref()
            .is_some_and(|rule| !is_empty_rule(rule))
        {
            log::warn!("Flag {key} has targeting rules, which aren't evaluated; using its default");
        }
        let Some(default_variant) = flag.default_variant else {
            log::warn!("Flag {key} has no defaultVariant; ignoring it");
            continue;
        };
        match flag.variants.get(&default_variant) {
            Some(serde_json::Value::Bool(value)) => {
                values.insert(key, *value);
            }
            Some(_) => log::warn!("Flag {key} isn't a boolean flag; ignoring it"),
            None => log::warn!("Flag {key} has no variant named {default_variant}; ignoring it"),
        }
    }
    Ok(values)
}

fn is_empty_rule(rule: &serde_json::Value) -> bool {
    match rule {
        serde_json::Value::Null => true,
        serde_json::Value::Object(map) => map.is_empty(),
        serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_)
        | serde_json::Value::Array(_) => false,
    }
}

/// The resolved value of every product area.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgFlags {
    values: BTreeMap<ProductArea, bool>,
}

impl OrgFlags {
    /// Loads the bundled defaults, then the admin file next to `organization.toml`, then (with
    /// `allow_overrides`, for development builds) the file named by [`FLAGS_FILE_ENV`]. A file
    /// that can't be read or parsed is skipped with an error in the log.
    pub fn load(allow_overrides: bool) -> Self {
        let mut flags = Self::bundled_defaults();
        let override_path = allow_overrides
            .then(|| std::env::var_os(FLAGS_FILE_ENV).map(PathBuf::from))
            .flatten();
        for path in admin_flags_path().into_iter().chain(override_path) {
            match read_optional(&path) {
                Ok(Some(json)) => flags.apply_source(&path.display().to_string(), &json),
                Ok(None) => {}
                Err(error) => log::error!("Couldn't read flags file {}: {error:#}", path.display()),
            }
        }
        flags
    }

    /// The defaults shipped with the app.
    pub fn bundled_defaults() -> Self {
        let mut flags = Self {
            values: ProductArea::ALL
                .into_iter()
                .map(|area| (area, true))
                .collect(),
        };
        flags.apply_source("bundled defaults", BUNDLED_DEFAULTS);
        flags
    }

    /// Layers a flag file over the current values, flag by flag. An invalid file changes nothing.
    pub fn apply_source(&mut self, label: &str, json: &str) {
        let values = match evaluate(json) {
            Ok(values) => values,
            Err(error) => {
                log::error!("Ignoring flags from {label}: {error:#}");
                return;
            }
        };
        for (key, value) in values {
            match ProductArea::from_key(&key) {
                Some(area) => {
                    self.values.insert(area, value);
                }
                None => log::warn!("Ignoring unknown flag {key} from {label}"),
            }
        }
    }

    pub fn is_enabled(&self, area: ProductArea) -> bool {
        self.values.get(&area).copied().unwrap_or(true)
    }
}

fn read_optional(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The system-wide location IT deploys the flag file to, next to `organization.toml`.
pub fn admin_flags_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some(PathBuf::from("/Library/Application Support/Warp").join(ADMIN_FLAGS_FILE_NAME))
    } else if cfg!(target_os = "windows") {
        std::env::var_os("ProgramData").map(|program_data| {
            PathBuf::from(program_data)
                .join("Warp")
                .join(ADMIN_FLAGS_FILE_NAME)
        })
    } else {
        Some(PathBuf::from("/etc/warp").join(ADMIN_FLAGS_FILE_NAME))
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
