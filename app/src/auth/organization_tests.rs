use org_flags::{OrgFlags, ProductArea};
use warp_core::features::{FeatureFlag, PRODUCT_AREA_FLAGS};

use super::{feature_flags_for, flags_to_disable};

fn flags_with(area: ProductArea, enabled: bool) -> OrgFlags {
    let mut flags = OrgFlags::bundled_defaults();
    let variant = if enabled { "on" } else { "off" };
    let key = area.key();
    flags.apply_source(
        "test",
        &format!(
            r#"{{"flags": {{"{key}": {{"state": "ENABLED", "variants": {{"on": true, "off": false}}, "defaultVariant": "{variant}"}}}}}}"#
        ),
    );
    flags
}

#[test]
fn product_areas_match_product_area_flags() {
    let mapped: Vec<FeatureFlag> = ProductArea::ALL
        .into_iter()
        .map(|area| feature_flags_for(area).0)
        .collect();
    assert_eq!(mapped, PRODUCT_AREA_FLAGS);
}

#[test]
fn turning_an_area_off_disables_its_upstream_flags() {
    for area in ProductArea::ALL {
        let (flag, upstream_flags) = feature_flags_for(area);
        let disabled = flags_to_disable(&flags_with(area, false));
        assert!(disabled.contains(&flag), "{area:?}");
        for upstream in upstream_flags {
            assert!(disabled.contains(upstream), "{area:?} {upstream:?}");
        }

        let disabled = flags_to_disable(&flags_with(area, true));
        assert!(!disabled.contains(&flag), "{area:?}");
    }
}

#[test]
fn codebase_indexing_off_disables_embedding_flags() {
    let disabled = flags_to_disable(&flags_with(ProductArea::CodebaseIndexing, false));
    for flag in [
        FeatureFlag::FullSourceCodeEmbedding,
        FeatureFlag::CodebaseIndexPersistence,
        FeatureFlag::CrossRepoContext,
    ] {
        assert!(disabled.contains(&flag), "{flag:?}");
    }
}

#[test]
fn voice_input_off_disables_hands_free() {
    let disabled = flags_to_disable(&flags_with(ProductArea::VoiceInput, false));
    assert!(disabled.contains(&FeatureFlag::VoiceHandsFree));
    assert!(disabled.contains(&FeatureFlag::VoiceConversation));
}

#[test]
fn hands_free_off_disables_conversation_mode() {
    let disabled = flags_to_disable(&flags_with(ProductArea::VoiceHandsFree, false));
    assert!(disabled.contains(&FeatureFlag::VoiceConversation));
}
