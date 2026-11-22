use super::sharing_entry_points_enabled;

#[test]
fn sharing_hidden_without_sharing_and_teams() {
    assert!(!sharing_entry_points_enabled(false, false, true));
    assert!(!sharing_entry_points_enabled(true, false, true));
}

#[test]
fn conversation_sharing_requires_cloud_sync() {
    assert!(!sharing_entry_points_enabled(true, true, false));
    assert!(sharing_entry_points_enabled(true, true, true));
    assert!(sharing_entry_points_enabled(false, true, false));
}
