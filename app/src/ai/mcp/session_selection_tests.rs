use uuid::Uuid;
use warpui::EntityId;

use super::SessionMcpSelection;

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
