use serde::{Deserialize, Serialize};

use super::HistoryEntry;
use crate::terminal::model::session::SessionId;

/// Which slice of command history a history list shows.
#[derive(
    Debug,
    Copy,
    Clone,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(
    description = "Which commands the history menu shows.",
    rename_all = "snake_case"
)]
pub enum HistoryScope {
    /// Commands from every host, shell, and session.
    Global,
    /// Commands from the current shell host (user, hostname, and shell type). Remote sessions use
    /// the remote host.
    #[default]
    Host,
    /// Commands run in the current terminal session.
    Session,
    /// Commands run on the current host whose working directory matches the current one.
    Directory,
}

impl HistoryScope {
    /// Every scope, in the order the history menu presents them.
    pub const ALL: [HistoryScope; 4] = [
        HistoryScope::Global,
        HistoryScope::Host,
        HistoryScope::Session,
        HistoryScope::Directory,
    ];

    /// The scope after this one in [`Self::ALL`], wrapping around.
    pub fn next(self) -> Self {
        match self {
            HistoryScope::Global => HistoryScope::Host,
            HistoryScope::Host => HistoryScope::Session,
            HistoryScope::Session => HistoryScope::Directory,
            HistoryScope::Directory => HistoryScope::Global,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HistoryScope::Global => "Global",
            HistoryScope::Host => "Host",
            HistoryScope::Session => "Session",
            HistoryScope::Directory => "Directory",
        }
    }

    /// Whether `entry`, already drawn from this scope's candidate set, should be kept.
    ///
    /// Entries without the metadata a scope needs (e.g. shell history file lines with no session
    /// or directory) are excluded from that scope.
    pub fn includes(
        self,
        entry: &HistoryEntry,
        session_id: SessionId,
        current_pwd: Option<&str>,
    ) -> bool {
        match self {
            HistoryScope::Global | HistoryScope::Host => true,
            HistoryScope::Session => entry.session_id == Some(session_id),
            HistoryScope::Directory => match (entry.pwd.as_deref(), current_pwd) {
                (Some(entry_pwd), Some(current_pwd)) => {
                    normalize_dir(entry_pwd) == normalize_dir(current_pwd)
                }
                _ => false,
            },
        }
    }
}

/// Who ran the commands a history list shows.
#[derive(
    Debug,
    Copy,
    Clone,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(
    description = "Whether the history menu shows commands typed by the user, run by the agent, or both.",
    rename_all = "snake_case"
)]
pub enum HistorySource {
    /// Commands typed by the user and commands run by the agent.
    #[default]
    All,
    /// Only commands the user ran.
    Me,
    /// Only commands the agent ran.
    Agent,
}

impl HistorySource {
    /// Every source, in the order the history menu presents them.
    pub const ALL: [HistorySource; 3] =
        [HistorySource::All, HistorySource::Me, HistorySource::Agent];

    /// The source after this one in [`Self::ALL`], wrapping around.
    pub fn next(self) -> Self {
        match self {
            HistorySource::All => HistorySource::Me,
            HistorySource::Me => HistorySource::Agent,
            HistorySource::Agent => HistorySource::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HistorySource::All => "All",
            HistorySource::Me => "Me",
            HistorySource::Agent => "Agent",
        }
    }

    pub fn includes(self, entry: &HistoryEntry) -> bool {
        match self {
            HistorySource::All => true,
            HistorySource::Me => !entry.is_agent_executed,
            HistorySource::Agent => entry.is_agent_executed,
        }
    }
}

/// Whether `command` invokes one of the helper functions Warp's shell integration defines (all
/// named `warp_*`, `_warp_*`, or `__warp_*`). Shells record these in their history when Warp runs
/// them, e.g. `warp_run_external_ctrl_r_widget 0`, but the user never typed them.
pub fn is_warp_internal_command(command: &str) -> bool {
    let first_word = command.split_whitespace().next().unwrap_or_default();
    first_word
        .trim_start_matches('_')
        .strip_prefix("warp_")
        .is_some_and(|rest| !rest.is_empty())
}

fn normalize_dir(dir: &str) -> &str {
    let trimmed = dir.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() { dir } else { trimmed }
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
