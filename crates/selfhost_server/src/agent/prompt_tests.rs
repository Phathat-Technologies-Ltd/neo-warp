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

    assert!(system_prompt(None, true, false).contains("<spoken>"));
    assert!(!system_prompt(None, false, false).contains("<spoken>"));
}

#[test]
fn spoken_summary_prompt_forbids_unspeakable_content() {
    let prompt = system_prompt(None, true, false);
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

    assert!(system_prompt(None, false, true).contains("came from speech recognition"));
    assert!(!system_prompt(None, true, false).contains("speech recognition"));
}

#[test]
fn sanitize_spoken_drops_code_paths_urls_and_flags() {
    assert_eq!(
        sanitize_spoken(
            "Run `cargo build --release` in ~/dev/warp, see https://x.io or ./out.log, use -v."
        ),
        "Run in see or use"
    );
    assert_eq!(
        sanitize_spoken("The build passed, about a day ago."),
        "The build passed, about a day ago."
    );
}
