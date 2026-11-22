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

fn normalize_dir(dir: &str) -> &str {
    let trimmed = dir.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() { dir } else { trimmed }
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
