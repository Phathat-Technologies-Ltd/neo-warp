use super::{HistoryTab, build_tab_configs};
use crate::terminal::history::HistoryScope;

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
