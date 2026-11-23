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

#[test]
fn idle_timeout_zero_means_never() {
    assert_eq!(idle_timeout(0), None);
    assert_eq!(idle_timeout(5), Some(Duration::from_secs(300)));
}

#[test]
fn idle_check_expires_only_after_the_timeout_since_the_last_activity() {
    let start = Instant::now();
    let timeout = Duration::from_secs(300);
    assert_eq!(
        check_idle(start, timeout, start + Duration::from_secs(10)),
        IdleCheck::CheckAt(start + timeout)
    );
    assert_eq!(
        check_idle(start, timeout, start + timeout),
        IdleCheck::Expired
    );
    // Activity resets the clock.
    let later = start + Duration::from_secs(200);
    assert_eq!(
        check_idle(later, timeout, start + timeout),
        IdleCheck::CheckAt(later + timeout)
    );
}

#[test]
fn idle_ended_message_names_the_timeout() {
    assert_eq!(
        idle_ended_message(5),
        "Voice conversation ended after 5 minutes without speech"
    );
    assert_eq!(
        idle_ended_message(1),
        "Voice conversation ended after 1 minute without speech"
    );
}
