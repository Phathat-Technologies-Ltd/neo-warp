use std::sync::Arc;

use rmcp::model::{Tool, ToolAnnotations};
use uuid::Uuid;

use super::*;

pub(crate) fn tool(name: &str, description: &str) -> Tool {
    Tool::new(
        name.to_owned(),
        description.to_owned(),
        Arc::new(Default::default()),
    )
}

fn annotated(
    name: &str,
    read_only: Option<bool>,
    destructive: Option<bool>,
    idempotent: Option<bool>,
    open_world: Option<bool>,
) -> Tool {
    tool(name, "").with_annotations(ToolAnnotations::from_raw(
        None,
        read_only,
        destructive,
        idempotent,
        open_world,
    ))
}

fn risk_and_source(tool: &Tool) -> (Option<McpToolRisk>, Option<McpToolRiskSource>) {
    let class = classify_tool(tool);
    (class.risk, class.source)
}

#[test]
fn annotations_win_over_the_tool_name() {
    let annotation = Some(McpToolRiskSource::Annotation);
    // Read-only even when open-world, and even if the name sounds destructive.
    assert_eq!(
        risk_and_source(&annotated(
            "delete_cache",
            Some(true),
            None,
            None,
            Some(true)
        )),
        (Some(McpToolRisk::ReadOnly), annotation)
    );
    // destructiveHint defaults to true for non-read-only tools.
    assert_eq!(
        risk_and_source(&annotated("get_thing", Some(false), None, None, None)),
        (Some(McpToolRisk::Destructive), annotation)
    );
    assert_eq!(
        risk_and_source(&annotated("x", None, Some(false), Some(true), Some(false))),
        (Some(McpToolRisk::Idempotent), annotation)
    );
    // openWorldHint defaults to true, so an idempotent tool without it is external.
    assert_eq!(
        risk_and_source(&annotated("x", None, Some(false), Some(true), None)),
        (Some(McpToolRisk::External), annotation)
    );
    assert_eq!(
        risk_and_source(&annotated("x", None, Some(false), Some(false), Some(false))),
        (Some(McpToolRisk::External), annotation)
    );
}

#[test]
fn empty_annotations_fall_back_to_the_heuristic() {
    assert_eq!(
        risk_and_source(&annotated("list_issues", None, None, None, None)),
        (
            Some(McpToolRisk::ReadOnly),
            Some(McpToolRiskSource::Heuristic)
        )
    );
}

#[test]
fn heuristic_reads_name_words_and_prefers_the_riskiest() {
    let risk = |name| classify_tool(&tool(name, "")).risk;
    assert_eq!(risk("list_issues"), Some(McpToolRisk::ReadOnly));
    assert_eq!(risk("getPullRequest"), Some(McpToolRisk::ReadOnly));
    assert_eq!(risk("update-issue"), Some(McpToolRisk::Idempotent));
    assert_eq!(risk("create_issue"), Some(McpToolRisk::External));
    assert_eq!(risk("sendSlackMessage"), Some(McpToolRisk::External));
    assert_eq!(risk("deleteRepo"), Some(McpToolRisk::Destructive));
    assert_eq!(risk("list_and_delete"), Some(McpToolRisk::Destructive));
    // Words must match whole, so "settings" isn't "set".
    assert_eq!(risk("settings"), None);
}

#[test]
fn heuristic_falls_back_to_the_description() {
    let class = classify_tool(&tool("issues", "Search issues in a repository."));
    assert_eq!(class.risk, Some(McpToolRisk::ReadOnly));
    assert_eq!(class.source, Some(McpToolRiskSource::Heuristic));
}

#[test]
fn heuristic_reads_the_description_even_when_the_name_looks_read_only() {
    let risk = |name, description| classify_tool(&tool(name, description)).risk;
    assert_eq!(
        risk("get_branch", "Gets a branch, or can delete it when stale."),
        Some(McpToolRisk::Destructive)
    );
    assert_eq!(
        risk("list_jobs", "Lists jobs and can trigger a rerun."),
        Some(McpToolRisk::External)
    );
    assert_eq!(
        risk("list_jobs", "Lists jobs in a project."),
        Some(McpToolRisk::ReadOnly)
    );
}

#[test]
fn unclassified_tools_are_treated_as_destructive() {
    let class = classify_tool(&tool("frobnicate", "Does a thing."));
    assert_eq!(class.risk, None);
    assert_eq!(class.label(), "Unclassified");
    assert_eq!(class.effective_risk(), McpToolRisk::Destructive);
    assert!(!McpAccessLevel::AllButDestructive.allows(&class));
    assert!(McpAccessLevel::All.allows(&class));
}

#[test]
fn user_overrides_win() {
    let server = Uuid::new_v4();
    let delete = tool("delete_branch", "");
    let mut overrides = McpToolRiskOverrides::new();
    overrides.insert(override_key(server, "delete_branch"), McpToolRisk::ReadOnly);
    let class = classify_tool_with_overrides(server, &delete, &overrides);
    assert_eq!(class, McpToolClass::user(McpToolRisk::ReadOnly));
    assert_eq!(class.label(), "Read-only (user)");
    // Overrides are per server.
    assert_eq!(
        classify_tool_with_overrides(Uuid::new_v4(), &delete, &overrides).risk,
        Some(McpToolRisk::Destructive)
    );
}

#[test]
fn levels_allow_classes_up_to_their_maximum() {
    let classes = [
        McpToolRisk::ReadOnly,
        McpToolRisk::Idempotent,
        McpToolRisk::External,
        McpToolRisk::Destructive,
    ]
    .map(McpToolClass::user);
    let allowed = |level: McpAccessLevel| classes.map(|class| level.allows(&class));
    assert_eq!(
        allowed(McpAccessLevel::ReadOnly),
        [true, false, false, false]
    );
    assert_eq!(
        allowed(McpAccessLevel::ReadAndIdempotent),
        [true, true, false, false]
    );
    assert_eq!(
        allowed(McpAccessLevel::AllButDestructive),
        [true, true, true, false]
    );
    assert_eq!(allowed(McpAccessLevel::All), [true, true, true, true]);
    assert_eq!(McpAccessLevel::default(), McpAccessLevel::All);
}

#[test]
fn cycling_user_risk_returns_to_automatic() {
    let mut risk = None;
    let mut seen = vec![];
    for _ in 0..5 {
        risk = next_user_risk(risk);
        seen.push(risk);
    }
    assert_eq!(
        seen,
        vec![
            Some(McpToolRisk::ReadOnly),
            Some(McpToolRisk::Idempotent),
            Some(McpToolRisk::External),
            Some(McpToolRisk::Destructive),
            None,
        ]
    );
}
