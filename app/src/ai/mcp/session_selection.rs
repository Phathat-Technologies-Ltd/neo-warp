//! Which of the user's MCP servers each terminal pane's agent may use.

use std::collections::HashMap;

use uuid::Uuid;
use warpui::EntityId;

/// The per-pane MCP server choices. A server the user hasn't toggled in a pane follows the
/// "MCP servers enabled in new sessions" setting.
#[derive(Debug, Default)]
pub struct SessionMcpSelection {
    panes: HashMap<EntityId, HashMap<Uuid, bool>>,
}

impl SessionMcpSelection {
    pub fn is_enabled(&self, pane: EntityId, server: Uuid, default_enabled: bool) -> bool {
        self.panes
            .get(&pane)
            .and_then(|servers| servers.get(&server))
            .copied()
            .unwrap_or(default_enabled)
    }

    pub fn set_enabled(&mut self, pane: EntityId, server: Uuid, enabled: bool) {
        self.panes.entry(pane).or_default().insert(server, enabled);
    }
}

#[cfg(test)]
#[path = "session_selection_tests.rs"]
mod tests;
