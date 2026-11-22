//! Risk classes for MCP tools and the per-session access levels that filter them.

use std::collections::HashMap;

use rmcp::model::Tool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How much harm calling an MCP tool can do, ordered from safest to riskiest.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(description = "Risk class of an MCP tool.", rename_all = "snake_case")]
pub enum McpToolRisk {
    ReadOnly,
    /// Changes state, but repeating the call with the same arguments has no further effect.
    Idempotent,
    /// Reaches outside systems, or makes changes that aren't safe to repeat.
    External,
    Destructive,
}

impl McpToolRisk {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read-only",
            Self::Idempotent => "Idempotent",
            Self::External => "External",
            Self::Destructive => "Destructive",
        }
    }
}

/// Where a tool's class came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpToolRiskSource {
    User,
    Annotation,
    Heuristic,
}

impl McpToolRiskSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Annotation => "annotation",
            Self::Heuristic => "heuristic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpToolClass {
    /// `None` when the tool couldn't be classified.
    pub risk: Option<McpToolRisk>,
    pub source: Option<McpToolRiskSource>,
}

impl McpToolClass {
    const UNCLASSIFIED: Self = Self {
        risk: None,
        source: None,
    };

    pub fn user(risk: McpToolRisk) -> Self {
        Self::new(risk, McpToolRiskSource::User)
    }

    fn new(risk: McpToolRisk, source: McpToolRiskSource) -> Self {
        Self {
            risk: Some(risk),
            source: Some(source),
        }
    }

    /// The risk used for filtering: unclassified tools get the most restrictive treatment.
    pub fn effective_risk(&self) -> McpToolRisk {
        self.risk.unwrap_or(McpToolRisk::Destructive)
    }

    pub fn label(&self) -> String {
        match (self.risk, self.source) {
            (Some(risk), Some(source)) => format!("{} ({})", risk.label(), source.label()),
            (Some(risk), None) => risk.label().to_owned(),
            (None, _) => "Unclassified".to_owned(),
        }
    }
}

/// User-chosen classes, keyed by [`override_key`].
pub type McpToolRiskOverrides = HashMap<String, McpToolRisk>;

pub fn override_key(server: Uuid, tool: &str) -> String {
    format!("{server}/{tool}")
}

pub fn classify_tool_with_overrides(
    server: Uuid,
    tool: &Tool,
    overrides: &McpToolRiskOverrides,
) -> McpToolClass {
    match overrides.get(&override_key(server, &tool.name)) {
        Some(risk) => McpToolClass::new(*risk, McpToolRiskSource::User),
        None => classify_tool(tool),
    }
}

/// Classifies a tool from its MCP annotations, falling back to words in its name and description.
pub fn classify_tool(tool: &Tool) -> McpToolClass {
    if let Some(risk) = tool.annotations.as_ref().and_then(|annotations| {
        classify_annotations(
            annotations.read_only_hint,
            annotations.destructive_hint,
            annotations.idempotent_hint,
            annotations.open_world_hint,
        )
    }) {
        return McpToolClass::new(risk, McpToolRiskSource::Annotation);
    }
    classify_words(&tool.name)
        .or_else(|| tool.description.as_deref().and_then(classify_words))
        .map_or(McpToolClass::UNCLASSIFIED, |risk| {
            McpToolClass::new(risk, McpToolRiskSource::Heuristic)
        })
}

/// Maps MCP annotations to a class, applying the spec's defaults for missing hints
/// (`destructiveHint` and `openWorldHint` default to true). Returns `None` when no hint is set.
fn classify_annotations(
    read_only: Option<bool>,
    destructive: Option<bool>,
    idempotent: Option<bool>,
    open_world: Option<bool>,
) -> Option<McpToolRisk> {
    if read_only.is_none() && destructive.is_none() && idempotent.is_none() && open_world.is_none()
    {
        return None;
    }
    Some(if read_only == Some(true) {
        McpToolRisk::ReadOnly
    } else if destructive != Some(false) {
        McpToolRisk::Destructive
    } else if idempotent == Some(true) && open_world == Some(false) {
        McpToolRisk::Idempotent
    } else {
        McpToolRisk::External
    })
}

const DESTRUCTIVE_WORDS: &[&str] = &[
    "delete",
    "del",
    "remove",
    "rm",
    "drop",
    "destroy",
    "purge",
    "truncate",
    "kill",
    "wipe",
    "erase",
    "overwrite",
    "revoke",
    "terminate",
    "uninstall",
    "reset",
    "force",
    "clear",
];
const EXTERNAL_WORDS: &[&str] = &[
    "send", "post", "create", "publish", "deploy", "push", "execute", "exec", "run", "email",
    "message", "comment", "merge", "submit", "invoke", "trigger", "upload", "notify", "reply",
    "invite", "transfer", "pay", "write", "insert", "add", "fork", "close", "start", "stop",
    "restart", "launch", "dispatch",
];
const IDEMPOTENT_WORDS: &[&str] = &[
    "set",
    "update",
    "put",
    "upsert",
    "enable",
    "disable",
    "rename",
    "assign",
    "unassign",
    "label",
    "tag",
    "edit",
    "modify",
    "patch",
    "configure",
    "move",
    "archive",
    "mark",
];
const READ_ONLY_WORDS: &[&str] = &[
    "get",
    "list",
    "read",
    "search",
    "fetch",
    "query",
    "describe",
    "show",
    "view",
    "find",
    "lookup",
    "count",
    "status",
    "info",
    "inspect",
    "browse",
    "retrieve",
    "check",
    "diff",
    "log",
    "logs",
    "download",
    "export",
    "explain",
    "preview",
    "summarize",
    "whoami",
    "ping",
];

/// Picks the riskiest class any word in `text` signals.
fn classify_words(text: &str) -> Option<McpToolRisk> {
    let words = split_words(text);
    let has_any = |list: &[&str]| words.iter().any(|word| list.contains(&word.as_str()));
    if has_any(DESTRUCTIVE_WORDS) {
        Some(McpToolRisk::Destructive)
    } else if has_any(EXTERNAL_WORDS) {
        Some(McpToolRisk::External)
    } else if has_any(IDEMPOTENT_WORDS) {
        Some(McpToolRisk::Idempotent)
    } else if has_any(READ_ONLY_WORDS) {
        Some(McpToolRisk::ReadOnly)
    } else {
        None
    }
}

/// Lowercase words, split on non-alphanumerics and lower-to-upper camelCase boundaries.
fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for ch in text.chars() {
        if !ch.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if ch.is_uppercase() && previous_lower && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        previous_lower = ch.is_lowercase() || ch.is_ascii_digit();
        current.extend(ch.to_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Which tool classes a session may use from a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpAccessLevel {
    ReadOnly,
    ReadAndIdempotent,
    AllButDestructive,
    #[default]
    All,
}

impl McpAccessLevel {
    fn max_risk(self) -> McpToolRisk {
        match self {
            Self::ReadOnly => McpToolRisk::ReadOnly,
            Self::ReadAndIdempotent => McpToolRisk::Idempotent,
            Self::AllButDestructive => McpToolRisk::External,
            Self::All => McpToolRisk::Destructive,
        }
    }

    pub fn allows(self, class: &McpToolClass) -> bool {
        class.effective_risk() <= self.max_risk()
    }

    pub fn next(self) -> Self {
        match self {
            Self::ReadOnly => Self::ReadAndIdempotent,
            Self::ReadAndIdempotent => Self::AllButDestructive,
            Self::AllButDestructive => Self::All,
            Self::All => Self::ReadOnly,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read-only",
            Self::ReadAndIdempotent => "Read + idempotent",
            Self::AllButDestructive => "All but destructive",
            Self::All => "All",
        }
    }
}

/// The class after `current` when the user cycles a tool's class in Settings; `None` returns the
/// tool to automatic classification.
pub fn next_user_risk(current: Option<McpToolRisk>) -> Option<McpToolRisk> {
    match current {
        None => Some(McpToolRisk::ReadOnly),
        Some(McpToolRisk::ReadOnly) => Some(McpToolRisk::Idempotent),
        Some(McpToolRisk::Idempotent) => Some(McpToolRisk::External),
        Some(McpToolRisk::External) => Some(McpToolRisk::Destructive),
        Some(McpToolRisk::Destructive) => None,
    }
}

#[cfg(test)]
#[path = "tool_risk_tests.rs"]
pub(crate) mod tests;
