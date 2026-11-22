//! Which of the user's MCP servers, and which of their tools, each terminal pane's agent may use.

use std::collections::HashMap;

use rmcp::model::Tool;
use uuid::Uuid;
use warpui::EntityId;

use crate::ai::mcp::tool_risk::{
    McpAccessLevel, McpToolRiskOverrides, classify_tool_with_overrides,
};

#[derive(Debug, Default, Clone, Copy)]
struct ServerChoice {
    /// `None` follows the "MCP servers enabled by default" setting.
    enabled: Option<bool>,
    /// `None` is [`McpAccessLevel::default`].
    level: Option<McpAccessLevel>,
}

/// The per-pane MCP server choices.
#[derive(Debug, Default)]
pub struct SessionMcpSelection {
    panes: HashMap<EntityId, HashMap<Uuid, ServerChoice>>,
}

impl SessionMcpSelection {
    fn choice(&self, pane: EntityId, server: Uuid) -> ServerChoice {
        self.panes
            .get(&pane)
            .and_then(|servers| servers.get(&server))
            .copied()
            .unwrap_or_default()
    }

    pub fn is_enabled(&self, pane: EntityId, server: Uuid, default_enabled: bool) -> bool {
        self.choice(pane, server).enabled.unwrap_or(default_enabled)
    }

    pub fn set_enabled(&mut self, pane: EntityId, server: Uuid, enabled: bool) {
        self.panes
            .entry(pane)
            .or_default()
            .entry(server)
            .or_default()
            .enabled = Some(enabled);
    }

    pub fn access_level(&self, pane: EntityId, server: Uuid) -> McpAccessLevel {
        self.choice(pane, server).level.unwrap_or_default()
    }

    pub fn set_access_level(&mut self, pane: EntityId, server: Uuid, level: McpAccessLevel) {
        self.panes
            .entry(pane)
            .or_default()
            .entry(server)
            .or_default()
            .level = Some(level);
    }

    /// The servers `pane`'s agent may use, each with the tools its access level allows. Without a
    /// pane, every server follows `default_enabled` with the default level.
    pub fn select<'a>(
        &self,
        pane: Option<EntityId>,
        default_enabled: bool,
        servers: impl IntoIterator<Item = (Uuid, &'a [Tool])>,
        overrides: &McpToolRiskOverrides,
    ) -> Vec<(Uuid, Vec<Tool>)> {
        servers
            .into_iter()
            .filter_map(|(server, tools)| {
                let choice = pane
                    .map(|pane| self.choice(pane, server))
                    .unwrap_or_default();
                if !choice.enabled.unwrap_or(default_enabled) {
                    return None;
                }
                let level = choice.level.unwrap_or_default();
                let tools = tools
                    .iter()
                    .filter(|tool| {
                        level.allows(&classify_tool_with_overrides(server, tool, overrides))
                    })
                    .cloned()
                    .collect();
                Some((server, tools))
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "session_selection_tests.rs"]
mod tests;
