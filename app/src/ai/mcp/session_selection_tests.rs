use uuid::Uuid;
use warpui::EntityId;

use super::{SessionMcpDefaults, SessionMcpSelection};
use crate::ai::mcp::tool_risk::tests::tool;
use crate::ai::mcp::tool_risk::{McpAccessLevel, McpToolRisk, McpToolRiskOverrides, override_key};

const ALL_ON: SessionMcpDefaults = SessionMcpDefaults {
    enabled: true,
    level: McpAccessLevel::All,
};
const ALL_OFF: SessionMcpDefaults = SessionMcpDefaults {
    enabled: false,
    level: McpAccessLevel::All,
};

fn names(tools: &[rmcp::model::Tool]) -> Vec<&str> {
    tools.iter().map(|tool| tool.name.as_ref()).collect()
}

#[test]
fn untouched_servers_follow_the_default() {
    let selection = SessionMcpSelection::default();
    let pane = EntityId::new();
    let server = Uuid::new_v4();
    assert!(selection.is_enabled(pane, server, ALL_ON));
    assert!(!selection.is_enabled(pane, server, ALL_OFF));
}

#[test]
fn a_started_session_keeps_the_defaults_it_started_with() {
    let mut selection = SessionMcpSelection::default();
    let pane = EntityId::new();
    let server = Uuid::new_v4();
    selection.start_session(
        pane,
        SessionMcpDefaults {
            enabled: true,
            level: McpAccessLevel::ReadOnly,
        },
    );
    // Starting again (e.g. a second footer for the pane) doesn't replace the snapshot.
    selection.start_session(pane, ALL_OFF);

    assert!(selection.is_enabled(pane, server, ALL_OFF));
    assert_eq!(
        selection.access_level(pane, server, ALL_OFF),
        McpAccessLevel::ReadOnly
    );
    assert!(!selection.is_enabled(EntityId::new(), server, ALL_OFF));
}

#[test]
fn a_toggle_overrides_the_default_only_in_its_pane() {
    let mut selection = SessionMcpSelection::default();
    let pane = EntityId::new();
    let other_pane = EntityId::new();
    let server = Uuid::new_v4();
    let other_server = Uuid::new_v4();

    selection.set_enabled(pane, server, false);
    assert!(!selection.is_enabled(pane, server, ALL_ON));
    assert!(selection.is_enabled(pane, other_server, ALL_ON));
    assert!(selection.is_enabled(other_pane, server, ALL_ON));

    selection.set_enabled(other_pane, server, true);
    assert!(selection.is_enabled(other_pane, server, ALL_OFF));
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

    let selected = selection.select(Some(pane), ALL_ON, servers, &overrides);
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
    let selected = selection.select(Some(pane), ALL_OFF, servers, &overrides);
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
            .select(None, ALL_OFF, [(server, tools.as_slice())], &overrides)
            .is_empty()
    );
    let selected = selection.select(None, ALL_ON, [(server, tools.as_slice())], &overrides);
    assert_eq!(names(&selected[0].1), vec!["delete_issue"]);
}

#[test]
fn tool_calls_to_a_disabled_server_are_rejected() {
    let server = Uuid::new_v4();
    let pane = EntityId::new();
    let list = tool("list_issues", "");
    let mut selection = SessionMcpSelection::default();
    selection.set_enabled(pane, server, false);
    let overrides = McpToolRiskOverrides::new();

    let result = selection.check_tool_call(
        Some(pane),
        ALL_ON,
        server,
        "list_issues",
        Some(&list),
        &overrides,
    );
    assert_eq!(
        result,
        Err("its MCP server is disabled in this session".to_owned())
    );
}

#[test]
fn tool_calls_above_the_access_level_are_rejected() {
    let server = Uuid::new_v4();
    let pane = EntityId::new();
    let list = tool("list_issues", "");
    let delete = tool("delete_issue", "");
    let mut selection = SessionMcpSelection::default();
    selection.set_access_level(pane, server, McpAccessLevel::ReadOnly);
    let mut overrides = McpToolRiskOverrides::new();
    let check = |name: &str, tool, overrides: &McpToolRiskOverrides| {
        selection.check_tool_call(Some(pane), ALL_ON, server, name, Some(tool), overrides)
    };

    assert_eq!(check("list_issues", &list, &overrides), Ok(()));
    assert_eq!(
        check("delete_issue", &delete, &overrides),
        Err(
            "delete_issue is destructive but this session's access level for its MCP server is \
             read-only"
                .to_owned()
        )
    );
    overrides.insert(override_key(server, "delete_issue"), McpToolRisk::ReadOnly);
    assert_eq!(check("delete_issue", &delete, &overrides), Ok(()));
}

#[test]
fn unclassified_and_unadvertised_tool_calls_need_the_all_level() {
    let server = Uuid::new_v4();
    let pane = EntityId::new();
    let frobnicate = tool("frobnicate", "");
    let mut selection = SessionMcpSelection::default();
    selection.set_access_level(pane, server, McpAccessLevel::AllButDestructive);
    let overrides = McpToolRiskOverrides::new();

    for tool in [Some(&frobnicate), None] {
        let result =
            selection.check_tool_call(Some(pane), ALL_ON, server, "frobnicate", tool, &overrides);
        assert_eq!(
            result,
            Err(
                "frobnicate is unclassified but this session's access level for its MCP server \
                 is all but destructive"
                    .to_owned()
            )
        );
    }

    selection.set_access_level(pane, server, McpAccessLevel::All);
    assert_eq!(
        selection.check_tool_call(Some(pane), ALL_ON, server, "frobnicate", None, &overrides),
        Ok(())
    );
}
