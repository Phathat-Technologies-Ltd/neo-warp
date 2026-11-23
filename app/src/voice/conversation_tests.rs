use super::*;

#[test]
fn common_prefix_respects_char_boundaries() {
    assert_eq!(common_prefix_len("hello", "hello world"), 5);
    assert_eq!(common_prefix_len("hello world", "hello"), 5);
    assert_eq!(common_prefix_len("abc", "abd"), 2);
    assert_eq!(common_prefix_len("café ok", "café no"), 6);
    assert_eq!(common_prefix_len("é", "è"), 0);
    assert_eq!(common_prefix_len("", "x"), 0);
}

#[test]
fn replies_follow_the_hands_free_target() {
    let (left, right) = (EntityId::from_usize(1), EntityId::from_usize(2));
    assert!(surface_changes(None, Some(left)));
    assert!(surface_changes(Some(left), Some(right)));
    assert!(!surface_changes(Some(left), Some(left)));
    assert!(!surface_changes(Some(left), None));
}

#[test]
fn pocket_speaks_with_its_only_voice() {
    let voices = new_speaker(SpeechOutputEngine::Pocket).voices();
    assert_eq!(
        choose_voice(&voices, "af_heart").map(|voice| voice.as_str().to_owned()),
        Some("alba".to_owned())
    );
    let kokoro = new_speaker(SpeechOutputEngine::Kokoro).voices();
    assert_eq!(
        choose_voice(&kokoro, "").map(|voice| voice.as_str().to_owned()),
        kokoro.first().map(|voice| voice.id.as_str().to_owned())
    );
}
