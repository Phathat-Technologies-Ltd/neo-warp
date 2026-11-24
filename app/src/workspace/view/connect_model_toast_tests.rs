use super::should_show_connect_model_toast;

#[test]
fn shows_once_for_a_signed_in_organization_user_without_a_credential() {
    assert!(should_show_connect_model_toast(true, true, false, false));
}

#[test]
fn hidden_when_a_credential_is_connected() {
    assert!(!should_show_connect_model_toast(true, true, true, false));
}

#[test]
fn hidden_after_it_was_shown_once() {
    assert!(!should_show_connect_model_toast(true, true, false, true));
}

#[test]
fn hidden_outside_organization_builds_and_before_sign_in() {
    assert!(!should_show_connect_model_toast(false, true, false, false));
    assert!(!should_show_connect_model_toast(true, false, false, false));
}
