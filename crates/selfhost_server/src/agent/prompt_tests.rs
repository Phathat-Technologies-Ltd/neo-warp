use std::collections::HashMap;

use super::*;

fn metadata(voice: Option<bool>) -> api::request::Metadata {
    let mut logging = HashMap::new();
    if let Some(voice) = voice {
        logging.insert(
            VOICE_CONVERSATION_METADATA_KEY.to_owned(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::BoolValue(voice)),
            },
        );
    }
    api::request::Metadata {
        logging,
        ..Default::default()
    }
}

#[test]
fn spoken_summary_only_when_conversation_mode_is_flagged() {
    assert!(wants_spoken_summary(Some(&metadata(Some(true)))));
    assert!(!wants_spoken_summary(Some(&metadata(Some(false)))));
    assert!(!wants_spoken_summary(Some(&metadata(None))));
    assert!(!wants_spoken_summary(None));

    assert!(system_prompt(None, true).contains("<spoken>"));
    assert!(!system_prompt(None, false).contains("<spoken>"));
}
