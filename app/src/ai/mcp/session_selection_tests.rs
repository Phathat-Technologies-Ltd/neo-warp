use uuid::Uuid;
use warpui::EntityId;

use super::SessionMcpSelection;
use crate::ai::mcp::tool_risk::tests::tool;
use crate::ai::mcp::tool_risk::{McpAccessLevel, McpToolRiskOverrides};

fn names(tools: &[rmcp::model::Tool]) -> Vec<&str> {
    tools.iter().map(|tool| tool.name.as_ref()).collect()
}

#[test]
fn untouched_servers_follow_the_default() {
    let selection = SessionMcpSelection::default();
    let pane = EntityId::new();
    let server = Uuid::new_v4();
    assert!(selection.is_enabled(pane, server, true));
    assert!(!selection.is_enabled(pane, server, false));
}

#[test]
fn a_toggle_overrides_the_default_only_in_its_pane() {
    let mut selection = SessionMcpSelection::default();
    let pane = EntityId::new();
    let other_pane = EntityId::new();
    let server = Uuid::new_v4();
    let other_server = Uuid::new_v4();

    selection.set_enabled(pane, server, false);
    assert!(!selection.is_enabled(pane, server, true));
    assert!(selection.is_enabled(pane, other_server, true));
    assert!(selection.is_enabled(other_pane, server, true));

    selection.set_enabled(other_pane, server, true);
    assert!(selection.is_enabled(other_pane, server, false));
}

#[test]
fn select_drops_disabled_servers_and_filters_tools_by_level() {
    let tools = vec![
        tool("list_issues", ""),
        tool("update_issue", ""),
        tool("create_issue", ""),
        tool("delete_issue", ""),
        tool("frobnicate", ""),
    ];
    let github = Uuid::new_v4();
    let linear = Uuid::new_v4();
    let slack = Uuid::new_v4();
    let pane = EntityId::new();
    let mut selection = SessionMcpSelection::default();
    selection.set_enabled(pane, linear, false);
    selection.set_access_level(pane, github, McpAccessLevel::ReadOnly);
    selection.set_access_level(pane, slack, McpAccessLevel::AllButDestructive);
    let servers = [
        (github, tools.as_slice()),
        (linear, tools.as_slice()),
        (slack, tools.as_slice()),
    ];
    let overrides = McpToolRiskOverrides::new();

    let selected = selection.select(Some(pane), true, servers, &overrides);
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].0, github);
    assert_eq!(names(&selected[0].1), vec!["list_issues"]);
    assert_eq!(selected[1].0, slack);
    assert_eq!(
        names(&selected[1].1),
        vec!["list_issues", "update_issue", "create_issue"]
    );

    // With the default off, only servers turned on in the pane remain.
    selection.set_enabled(pane, slack, true);
    let selected = selection.select(Some(pane), false, servers, &overrides);
    assert_eq!(
        selected.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![slack]
    );
}

#[test]
fn select_without_a_pane_follows_the_default() {
    let tools = vec![tool("delete_issue", "")];
    let server = Uuid::new_v4();
    let mut selection = SessionMcpSelection::default();
    selection.set_enabled(EntityId::new(), server, false);
    let overrides = McpToolRiskOverrides::new();

    assert!(
        selection
            .select(None, false, [(server, tools.as_slice())], &overrides)
            .is_empty()
    );
    let selected = selection.select(None, true, [(server, tools.as_slice())], &overrides);
    assert_eq!(names(&selected[0].1), vec!["delete_issue"]);
}
