use super::*;

fn flag(state: &str, default_variant: &str) -> String {
    format!(
        r#"{{"state": "{state}", "variants": {{"on": true, "off": false}}, "defaultVariant": "{default_variant}"}}"#
    )
}

#[test]
fn bundled_defaults_define_every_area() {
    let values = evaluate(BUNDLED_DEFAULTS).unwrap();
    for area in ProductArea::ALL {
        assert!(values.contains_key(area.key()), "{} missing", area.key());
    }
}

#[test]
fn bundled_defaults_turn_off_backend_features() {
    let flags = OrgFlags::bundled_defaults();
    for area in [
        ProductArea::WarpDrive,
        ProductArea::SettingsSync,
        ProductArea::CloudAgents,
        ProductArea::WarpHostedMcp,
        ProductArea::SharingAndTeams,
        ProductArea::BillingAndReferrals,
        ProductArea::BlockTitles,
        ProductArea::ConversationCloudSync,
    ] {
        assert!(!flags.is_enabled(area), "{area:?}");
    }
    for area in [
        ProductArea::VoiceInput,
        ProductArea::VoiceHandsFree,
        ProductArea::VoiceConversation,
        ProductArea::CodebaseIndexing,
        ProductArea::AiPromptSuggestions,
        ProductArea::AiCommandSearch,
    ] {
        assert!(flags.is_enabled(area), "{area:?}");
    }
}

#[test]
fn evaluates_default_variant() {
    let json = format!(
        r#"{{"$schema": "https://flagd.dev/schema/v0/flags.json", "flags": {{"a": {}, "b": {}}}}}"#,
        flag("ENABLED", "on"),
        flag("ENABLED", "off"),
    );
    let values = evaluate(&json).unwrap();
    assert_eq!(values.get("a"), Some(&true));
    assert_eq!(values.get("b"), Some(&false));
}

#[test]
fn disabled_flags_have_no_value() {
    let json = format!(r#"{{"flags": {{"a": {}}}}}"#, flag("DISABLED", "on"));
    assert!(evaluate(&json).unwrap().is_empty());
}

#[test]
fn targeting_falls_back_to_default_variant() {
    let json = r#"{"flags": {"a": {"state": "ENABLED", "variants": {"on": true, "off": false},
        "defaultVariant": "on", "targeting": {"if": [true, "off", "on"]}}}}"#;
    assert_eq!(evaluate(json).unwrap().get("a"), Some(&true));
}

#[test]
fn non_boolean_and_unknown_variants_are_ignored() {
    let json = r#"{"flags": {
        "s": {"state": "ENABLED", "variants": {"x": "str"}, "defaultVariant": "x"},
        "m": {"state": "ENABLED", "variants": {"on": true}, "defaultVariant": "nope"},
        "n": {"state": "ENABLED", "variants": {"on": true}}
    }}"#;
    assert!(evaluate(json).unwrap().is_empty());
}

#[test]
fn later_sources_override_flag_by_flag() {
    let mut flags = OrgFlags::bundled_defaults();
    let json = format!(
        r#"{{"flags": {{"warp_drive": {}, "voice_input": {}, "settings_sync": {}, "bogus": {}}}}}"#,
        flag("ENABLED", "on"),
        flag("ENABLED", "off"),
        flag("DISABLED", "on"),
        flag("ENABLED", "on"),
    );
    flags.apply_source("test", &json);
    assert!(flags.is_enabled(ProductArea::WarpDrive));
    assert!(!flags.is_enabled(ProductArea::VoiceInput));
    assert!(!flags.is_enabled(ProductArea::SettingsSync));
    assert!(!flags.is_enabled(ProductArea::CloudAgents));
}

#[test]
fn invalid_source_changes_nothing() {
    let mut flags = OrgFlags::bundled_defaults();
    flags.apply_source("test", "{not json");
    flags.apply_source("test", r#"{"flags": {"warp_drive": {"state": "MAYBE"}}}"#);
    assert_eq!(flags, OrgFlags::bundled_defaults());
}

#[test]
fn reads_missing_file_as_none() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        read_optional(&dir.path().join("absent.json"))
            .unwrap()
            .is_none()
    );
    let path = dir.path().join("flags.json");
    std::fs::write(&path, "{}").unwrap();
    assert_eq!(read_optional(&path).unwrap().as_deref(), Some("{}"));
}
