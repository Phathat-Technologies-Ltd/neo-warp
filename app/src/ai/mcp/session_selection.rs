//! Which of the user's MCP servers, and which of their tools, each terminal pane's agent may use.

use std::collections::HashMap;

use rmcp::model::Tool;
use uuid::Uuid;
use warpui::EntityId;

use crate::ai::mcp::tool_risk::{
    McpAccessLevel, McpToolClass, McpToolRisk, McpToolRiskOverrides, classify_tool_with_overrides,
};

/// What a session uses for servers the user hasn't configured in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionMcpDefaults {
    pub enabled: bool,
    pub level: McpAccessLevel,
}

#[derive(Debug, Default, Clone, Copy)]
struct ServerChoice {
    /// `None` follows the session's [`SessionMcpDefaults::enabled`].
    enabled: Option<bool>,
    /// `None` follows the session's [`SessionMcpDefaults::level`].
    level: Option<McpAccessLevel>,
}

#[derive(Debug, Default)]
struct PaneSelection {
    /// Snapshotted when the session starts so later changes to the settings don't reach it.
    defaults: Option<SessionMcpDefaults>,
    servers: HashMap<Uuid, ServerChoice>,
}

/// The per-pane MCP server choices.
#[derive(Debug, Default)]
pub struct SessionMcpSelection {
    panes: HashMap<EntityId, PaneSelection>,
}

impl SessionMcpSelection {
    /// Records `defaults` for `pane` unless it already has them.
    pub fn start_session(&mut self, pane: EntityId, defaults: SessionMcpDefaults) {
        self.panes
            .entry(pane)
            .or_default()
            .defaults
            .get_or_insert(defaults);
    }

    /// The enabled state and access level of `server` in `pane`. `current_defaults` applies when
    /// there is no pane or the pane never started a session.
    fn resolve(
        &self,
        pane: Option<EntityId>,
        server: Uuid,
        current_defaults: SessionMcpDefaults,
    ) -> (bool, McpAccessLevel) {
        let selection = pane.and_then(|pane| self.panes.get(&pane));
        let defaults = selection
            .and_then(|selection| selection.defaults)
            .unwrap_or(current_defaults);
        let choice = selection
            .and_then(|selection| selection.servers.get(&server))
            .copied()
            .unwrap_or_default();
        (
            choice.enabled.unwrap_or(defaults.enabled),
            choice.level.unwrap_or(defaults.level),
        )
    }

    fn choice_mut(&mut self, pane: EntityId, server: Uuid) -> &mut ServerChoice {
        self.panes
            .entry(pane)
            .or_default()
            .servers
            .entry(server)
            .or_default()
    }

    pub fn is_enabled(
        &self,
        pane: EntityId,
        server: Uuid,
        current_defaults: SessionMcpDefaults,
    ) -> bool {
        self.resolve(Some(pane), server, current_defaults).0
    }

    pub fn set_enabled(&mut self, pane: EntityId, server: Uuid, enabled: bool) {
        self.choice_mut(pane, server).enabled = Some(enabled);
    }

    pub fn access_level(
        &self,
        pane: EntityId,
        server: Uuid,
        current_defaults: SessionMcpDefaults,
    ) -> McpAccessLevel {
        self.resolve(Some(pane), server, current_defaults).1
    }

    pub fn set_access_level(&mut self, pane: EntityId, server: Uuid, level: McpAccessLevel) {
        self.choice_mut(pane, server).level = Some(level);
    }

    /// The servers `pane`'s agent may use, each with the tools its access level allows.
    pub fn select<'a>(
        &self,
        pane: Option<EntityId>,
        current_defaults: SessionMcpDefaults,
        servers: impl IntoIterator<Item = (Uuid, &'a [Tool])>,
        overrides: &McpToolRiskOverrides,
    ) -> Vec<(Uuid, Vec<Tool>)> {
        servers
            .into_iter()
            .filter_map(|(server, tools)| {
                let (enabled, level) = self.resolve(pane, server, current_defaults);
                if !enabled {
                    return None;
                }
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

    /// Checks that `pane`'s agent may call `tool_name` on `server`, returning why not otherwise.
    /// `tool` is the server's definition of the tool, if it advertises one; a tool it doesn't
    /// advertise can't be classified and is treated as destructive.
    pub fn check_tool_call(
        &self,
        pane: Option<EntityId>,
        current_defaults: SessionMcpDefaults,
        server: Uuid,
        tool_name: &str,
        tool: Option<&Tool>,
        overrides: &McpToolRiskOverrides,
    ) -> Result<(), String> {
        let (enabled, level) = self.resolve(pane, server, current_defaults);
        if !enabled {
            return Err("its MCP server is disabled in this session".to_owned());
        }
        let class = tool.map_or(McpToolClass::UNCLASSIFIED, |tool| {
            classify_tool_with_overrides(server, tool, overrides)
        });
        if level.allows(&class) {
            Ok(())
        } else {
            let risk = class.risk.map_or("unclassified", McpToolRisk::label);
            Err(format!(
                "{tool_name} is {} but this session's access level for its MCP server is {}",
                risk.to_lowercase(),
                level.label().to_lowercase(),
            ))
        }
    }
}

#[cfg(test)]
#[path = "session_selection_tests.rs"]
mod tests;
