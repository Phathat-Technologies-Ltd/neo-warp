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

#[test]
fn blocked_notice_is_sent_once_until_the_setting_is_turned_back_on() {
    let notices = blocked_notices();
    set_system_echo_cancellation(true, false);
    on_blocked(BlockReason::Zeros, 0);
    assert!(notices.try_recv().is_ok());
    on_blocked(BlockReason::Zeros, 0);
    assert!(notices.try_recv().is_err());
    set_system_echo_cancellation(false, false);
    set_system_echo_cancellation(true, false);
    on_blocked(BlockReason::Zeros, 0);
    assert!(notices.try_recv().is_ok());
}

#[test]
fn reset_clears_a_blocked_tap_and_failures() {
    let mut selector = SystemAecSelector::default();
    selector.set_wanted(true);
    selector.on_blocked();
    assert!(selector.is_held_back());
    assert!(!selector.should_try(true));
    selector.reset();
    assert!(!selector.is_held_back());
    assert!(selector.should_try(true));

    selector.on_never_started();
    assert!(selector.is_held_back());
    selector.reset();
    assert!(selector.should_try(true));
}

#[test]
fn unwanted_pipeline_is_not_held_back() {
    let mut selector = SystemAecSelector::default();
    selector.on_blocked();
    assert!(!selector.is_held_back());
}
