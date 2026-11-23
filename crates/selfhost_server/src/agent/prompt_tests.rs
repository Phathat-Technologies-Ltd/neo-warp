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

    assert!(system_prompt(None, true, false, false).contains("<spoken>"));
    assert!(!system_prompt(None, false, false, false).contains("<spoken>"));
}

#[test]
fn spoken_summary_prompt_forbids_unspeakable_content() {
    let prompt = system_prompt(None, true, false, false);
    for phrase in [
        "through speakers",
        "never include commands, shell syntax, flags, file paths",
        "\"about a day\", not 86400 seconds",
        "three digits",
        "what the user should do",
        "you have two uncommitted changes",
    ] {
        assert!(prompt.contains(phrase), "missing {phrase:?}");
    }
}

#[test]
fn dictated_instructions_follow_the_dictated_flag() {
    let mut dictated = metadata(None);
    dictated.logging.insert(
        DICTATED_METADATA_KEY.to_owned(),
        prost_types::Value {
            kind: Some(prost_types::value::Kind::BoolValue(true)),
        },
    );
    assert!(is_dictated(Some(&dictated)));
    assert!(!is_dictated(Some(&metadata(Some(true)))));
    assert!(!is_dictated(None));

    assert!(system_prompt(None, false, true, false).contains("came from speech recognition"));
    assert!(!system_prompt(None, true, false, false).contains("speech recognition"));
}

#[test]
fn voice_terms_instructions_follow_the_voice_terms_flag() {
    let mut flagged = metadata(None);
    flagged.logging.insert(
        VOICE_TERMS_METADATA_KEY.to_owned(),
        prost_types::Value {
            kind: Some(prost_types::value::Kind::BoolValue(true)),
        },
    );
    assert!(wants_voice_terms(Some(&flagged)));
    assert!(!wants_voice_terms(Some(&metadata(Some(true)))));

    let prompt = system_prompt(None, false, false, true);
    assert!(prompt.contains("<terms>"));
    assert!(prompt.contains("Never list common English words"));
    assert!(!system_prompt(None, true, true, false).contains("<terms>"));
}
