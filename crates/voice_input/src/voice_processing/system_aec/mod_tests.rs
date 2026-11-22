use std::time::Duration;

use super::*;

const TICK: Duration = Duration::from_millis(500);

#[test]
fn tap_with_sound_is_healthy_and_stays_so() {
    let mut detector = TapHealthDetector::default();
    assert_eq!(detector.observe(false, false, TICK), None);
    assert_eq!(detector.observe(true, true, TICK), Some(TapHealth::Healthy));
    for _ in 0..20 {
        assert_eq!(detector.observe(false, true, TICK), None);
    }
    assert_eq!(detector.health(), TapHealth::Healthy);
}

#[test]
fn silent_tap_is_only_blocked_after_three_seconds_of_playback() {
    let mut detector = TapHealthDetector::default();
    for _ in 0..100 {
        assert_eq!(detector.observe(false, false, TICK), None);
    }
    for _ in 0..5 {
        assert_eq!(detector.observe(false, true, TICK), None);
    }
    assert_eq!(detector.health(), TapHealth::Unknown);
    assert_eq!(
        detector.observe(false, true, TICK),
        Some(TapHealth::Blocked)
    );
    assert_eq!(detector.observe(true, true, TICK), None);
    assert_eq!(detector.health(), TapHealth::Blocked);
}

#[test]
fn playback_time_accumulates_across_pauses() {
    let mut detector = TapHealthDetector::default();
    for _ in 0..3 {
        detector.observe(false, true, TICK);
        detector.observe(false, false, TICK);
    }
    for _ in 0..2 {
        detector.observe(false, true, TICK);
    }
    assert_eq!(
        detector.observe(false, true, TICK),
        Some(TapHealth::Blocked)
    );
}

#[test]
fn selector_needs_the_setting_and_a_supported_os() {
    let mut selector = SystemAecSelector::default();
    assert!(!selector.should_try(true));
    selector.set_wanted(true);
    assert!(selector.should_try(true));
    assert!(!selector.should_try(false));
}

#[test]
fn selector_backs_off_after_failed_starts_until_devices_change() {
    let mut selector = SystemAecSelector::default();
    selector.set_wanted(true);
    selector.on_start_failed();
    assert!(selector.should_try(true));
    selector.on_start_failed();
    assert!(!selector.should_try(true));
    selector.on_devices_changed();
    assert!(selector.should_try(true));
}

#[test]
fn blocked_tap_disables_until_the_setting_is_turned_back_on() {
    let mut selector = SystemAecSelector::default();
    selector.set_wanted(true);
    selector.on_blocked();
    assert!(!selector.should_try(true));
    selector.on_devices_changed();
    assert!(!selector.should_try(true));
    assert!(!selector.set_wanted(true));
    assert!(!selector.should_try(true));
    selector.set_wanted(false);
    selector.set_wanted(true);
    assert!(selector.should_try(true));
}

#[test]
fn speech_plays_through_cpal_while_system_aec_runs() {
    assert_eq!(
        output_backend(true, AudioBackend::VoiceProcessing),
        AudioBackend::Cpal
    );
    assert_eq!(
        output_backend(false, AudioBackend::VoiceProcessing),
        AudioBackend::VoiceProcessing
    );
}
