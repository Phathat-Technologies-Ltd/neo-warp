use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use super::*;

#[test]
fn ring_rounds_capacity_up_to_a_power_of_two() {
    assert_eq!(SampleRing::new(5).capacity(), 8);
    assert_eq!(SampleRing::new(0).capacity(), 1);
}

#[test]
fn ring_pops_in_order_and_pads_with_silence() {
    let ring = SampleRing::new(8);
    assert_eq!(ring.push(&[0.1, 0.2, 0.3]), 3);
    let mut out = [9.0; 2];
    assert_eq!(ring.pop_into(&mut out), 2);
    assert_eq!(out, [0.1, 0.2]);
    let mut out = [9.0; 3];
    assert_eq!(ring.pop_into(&mut out), 3);
    assert_eq!(out, [0.3, 0.0, 0.0]);
}

#[test]
fn ring_refuses_samples_beyond_capacity_and_wraps_around() {
    let ring = SampleRing::new(4);
    assert_eq!(ring.push(&[1.0, 2.0, 3.0, 4.0, 5.0]), 4);
    let mut out = [0.0; 3];
    ring.pop_into(&mut out);
    assert_eq!(ring.push(&[5.0, 6.0, 7.0, 8.0]), 3);
    let mut out = [0.0; 4];
    ring.pop_into(&mut out);
    assert_eq!(out, [4.0, 5.0, 6.0, 7.0]);
}

#[test]
fn ring_flush_discards_queued_samples_but_not_later_ones() {
    let ring = SampleRing::new(8);
    ring.push(&[1.0; 6]);
    ring.flush();
    // The flush frees the whole ring for the producer before the consumer catches up.
    assert_eq!(ring.push(&[2.0; 8]), 8);
    let mut out = [0.0; 8];
    ring.pop_into(&mut out);
    assert_eq!(out, [2.0; 8]);
}

#[test]
fn ring_works_across_threads() {
    let ring = Arc::new(SampleRing::new(64));
    let producer = ring.clone();
    let total = 10_000;
    let handle = std::thread::spawn(move || {
        let mut next = 0;
        while next < total {
            let batch: Vec<f32> = (next..(next + 7).min(total)).map(|i| i as f32).collect();
            next += producer.push(&batch);
        }
    });
    let mut received = Vec::new();
    let mut out = [0.0; 5];
    let mut read = 0;
    while received.len() < total {
        let after = ring.pop_into(&mut out);
        received.extend_from_slice(&out[..after - read]);
        read = after;
    }
    handle.join().unwrap();
    assert!(received.iter().enumerate().all(|(i, &s)| s == i as f32));
}

#[test]
fn playback_reports_the_end_of_a_clip_once() {
    let queue = PlaybackQueue::new(16);
    let finished = Arc::new(AtomicUsize::new(0));
    let counter = finished.clone();
    queue.play(
        &[0.5, 0.5, 0.5],
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let mut out = [0.0; 2];
    queue.render(&mut out);
    assert_eq!(finished.load(Ordering::SeqCst), 0);
    queue.render(&mut out);
    assert_eq!(out, [0.5, 0.0]);
    queue.render(&mut out);
    assert_eq!(finished.load(Ordering::SeqCst), 1);
}

#[test]
fn stopped_or_replaced_clips_never_finish() {
    let queue = PlaybackQueue::new(16);
    let finished = Arc::new(AtomicUsize::new(0));
    let first = finished.clone();
    queue.play(
        &[0.1; 4],
        Box::new(move || {
            first.fetch_add(1, Ordering::SeqCst);
        }),
    );
    queue.stop();
    let mut out = [9.0; 4];
    queue.render(&mut out);
    assert_eq!(out, [0.0; 4]);
    assert_eq!(finished.load(Ordering::SeqCst), 0);

    let replaced = finished.clone();
    queue.play(
        &[0.1; 4],
        Box::new(move || {
            replaced.fetch_add(100, Ordering::SeqCst);
        }),
    );
    let second = finished.clone();
    queue.play(
        &[0.2; 2],
        Box::new(move || {
            second.fetch_add(1, Ordering::SeqCst);
        }),
    );
    queue.render(&mut out);
    assert_eq!(out, [0.2, 0.2, 0.0, 0.0]);
    assert_eq!(finished.load(Ordering::SeqCst), 1);
}

#[test]
fn selector_uses_voice_processing_only_when_wanted_and_supported() {
    let mut selector = BackendSelector::default();
    assert_eq!(selector.choose(true), AudioBackend::Cpal);
    assert!(selector.set_wanted(true));
    assert!(!selector.set_wanted(true));
    assert_eq!(selector.choose(true), AudioBackend::VoiceProcessing);
    assert_eq!(selector.choose(false), AudioBackend::Cpal);
}

#[test]
fn fallback_opens_cpal_when_the_unit_fails_and_backs_off() {
    let mut selector = BackendSelector::default();
    selector.set_wanted(true);
    let attempts = std::cell::Cell::new(0);
    let failing_unit = || {
        attempts.set(attempts.get() + 1);
        anyhow::bail!("no unit")
    };
    for _ in 0..MAX_START_FAILURES {
        let (stream, backend) =
            open_with_fallback(&mut selector, true, failing_unit, || Ok("cpal")).unwrap();
        assert_eq!((stream, backend), ("cpal", AudioBackend::Cpal));
    }
    // Further opens skip the broken unit.
    open_with_fallback(&mut selector, true, failing_unit, || Ok("cpal")).unwrap();
    assert_eq!(attempts.get(), MAX_START_FAILURES);

    // Turning it off and on again retries the unit.
    selector.set_wanted(false);
    selector.set_wanted(true);
    let (stream, backend) =
        open_with_fallback(&mut selector, true, || Ok("unit"), || Ok("cpal")).unwrap();
    assert_eq!((stream, backend), ("unit", AudioBackend::VoiceProcessing));
}

#[test]
fn a_successful_start_resets_failures() {
    let mut selector = BackendSelector::default();
    selector.set_wanted(true);
    selector.on_start_failed();
    selector.on_started(AudioBackend::VoiceProcessing);
    selector.on_start_failed();
    assert_eq!(selector.choose(true), AudioBackend::VoiceProcessing);
}

#[test]
fn fallback_errors_when_cpal_fails_too() {
    let mut selector = BackendSelector::default();
    selector.set_wanted(true);
    let result = open_with_fallback::<()>(
        &mut selector,
        true,
        || anyhow::bail!("no unit"),
        || anyhow::bail!("no device"),
    );
    assert!(result.is_err());
}

#[test]
fn reopen_reaches_registered_captures_until_dropped() {
    let (tx, rx) = mpsc::channel();
    let registration = CaptureRegistration::new(tx);
    reopen_captures();
    assert_eq!(rx.try_recv(), Ok(Signal::Reopen));
    drop(registration);
    reopen_captures();
    assert!(rx.try_recv().is_err());
}

#[test]
fn speaker_moves_onto_the_unit_when_system_aec_stops() {
    let mut selector = BackendSelector::default();
    selector.set_wanted(true);
    assert_eq!(
        desired_output_backend(&selector, true, true),
        AudioBackend::Cpal
    );
    assert_eq!(
        desired_output_backend(&selector, true, false),
        AudioBackend::VoiceProcessing
    );
    selector.set_wanted(false);
    assert_eq!(
        desired_output_backend(&selector, true, false),
        AudioBackend::Cpal
    );
}

#[test]
fn backend_changes_bump_the_generation_and_notify() {
    let changes = backend_changes();
    let before = backend_generation();
    notify_backend_changed();
    notify_backend_changed();
    assert!(backend_generation() >= before + 2);
    assert!(changes.try_recv().is_ok());
    // Bursts are coalesced into one pending message.
    assert!(changes.try_recv().is_err());
    drop(changes);
    notify_backend_changed();
    assert!(BACKEND_LISTENERS.lock().iter().all(|tx| !tx.is_closed()));
}
