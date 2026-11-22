use super::{
    HistoryTab, build_tab_configs, scope_selector_label, source_selector_label,
    tabs_select_item_type,
};
use crate::terminal::history::{HistoryScope, HistorySource};

#[test]
fn terminal_mode_tabs_are_the_history_scopes() {
    let tabs = build_tab_configs(false);
    assert_eq!(
        tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
        HistoryScope::ALL
            .into_iter()
            .map(HistoryTab::Scope)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        tabs.iter()
            .map(|tab| tab.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Global", "Host", "Session", "Directory"]
    );
    assert!(tabs.iter().all(|tab| tab.filters.is_empty()));
}

#[test]
fn agent_view_tabs_select_item_type() {
    let tabs = build_tab_configs(true);
    assert_eq!(
        tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
        vec![HistoryTab::All, HistoryTab::Commands, HistoryTab::Prompts]
    );
}

#[test]
fn scope_selector_only_shows_when_tabs_select_item_type() {
    assert!(tabs_select_item_type(&build_tab_configs(true)));
    assert!(!tabs_select_item_type(&build_tab_configs(false)));
}

#[test]
fn scope_selector_label_names_scope() {
    assert_eq!(scope_selector_label(HistoryScope::Host), "Scope: Host");
    assert_eq!(
        scope_selector_label(HistoryScope::Directory),
        "Scope: Directory"
    );
}

#[test]
fn cycling_scope_visits_every_scope_in_menu_order() {
    let mut scope = HistoryScope::Global;
    let mut visited = vec![scope];
    for _ in 1..HistoryScope::ALL.len() {
        scope = scope.next();
        visited.push(scope);
    }
    assert_eq!(visited, HistoryScope::ALL.to_vec());
    assert_eq!(scope.next(), HistoryScope::Global);
}

#[test]
fn source_selector_label_names_source() {
    assert_eq!(source_selector_label(HistorySource::All), "Source: All");
    assert_eq!(source_selector_label(HistorySource::Me), "Source: Me");
    assert_eq!(source_selector_label(HistorySource::Agent), "Source: Agent");
}
