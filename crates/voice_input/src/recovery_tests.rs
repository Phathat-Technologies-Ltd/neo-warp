use std::sync::Mutex;

use super::*;

#[test]
fn glitches_are_ignored_and_failures_back_off_until_giving_up() {
    let mut policy = RecoveryPolicy::new(Duration::from_millis(100), 3);
    assert_eq!(
        policy.on_fault(StreamFault::Transient),
        RecoveryDecision::KeepRunning
    );
    assert_eq!(
        policy.on_fault(StreamFault::Fatal),
        RecoveryDecision::RebuildAfter(Duration::from_millis(100))
    );
    assert_eq!(
        policy.on_failure(),
        RecoveryDecision::RebuildAfter(Duration::from_millis(200))
    );
    assert_eq!(
        policy.on_failure(),
        RecoveryDecision::RebuildAfter(Duration::from_millis(400))
    );
    assert_eq!(policy.on_failure(), RecoveryDecision::GiveUp);

    policy.on_healthy();
    assert_eq!(
        policy.on_fault(StreamFault::Fatal),
        RecoveryDecision::RebuildAfter(Duration::from_millis(100))
    );
}

#[test]
fn xruns_are_transient() {
    assert_eq!(
        StreamFault::classify(cpal::ErrorKind::Xrun),
        StreamFault::Transient
    );
    assert_eq!(
        StreamFault::classify(cpal::ErrorKind::DeviceNotAvailable),
        StreamFault::Fatal
    );
}

fn timing() -> RecoveryTiming {
    RecoveryTiming {
        stall_after: Duration::from_millis(40),
        first_retry_after: Duration::from_millis(5),
        max_rebuilds: 2,
    }
}

/// A fake stream: each open is recorded, and the test decides whether opens succeed.
struct FakeStreams {
    opens: Arc<Mutex<u32>>,
    fail_reopens: bool,
}

impl FakeStreams {
    fn open(&self, heartbeat: Heartbeat) -> anyhow::Result<Heartbeat> {
        let mut opens = self.opens.lock().unwrap();
        *opens += 1;
        if *opens > 1 && self.fail_reopens {
            anyhow::bail!("device gone");
        }
        Ok(heartbeat)
    }
}

#[test]
fn a_fatal_fault_rebuilds_the_stream_and_capture_continues() {
    let opens = Arc::new(Mutex::new(0));
    let streams = FakeStreams {
        opens: opens.clone(),
        fail_reopens: false,
    };
    let (tx, rx) = mpsc::channel();
    let failed = Arc::new(Mutex::new(None));
    let failed_in = failed.clone();
    let heartbeat_out = Arc::new(Mutex::new(None));
    let heartbeat_in = heartbeat_out.clone();
    let supervisor = std::thread::spawn(move || {
        supervise(
            |heartbeat| {
                *heartbeat_in.lock().unwrap() = Some(heartbeat.clone());
                streams.open(heartbeat)
            },
            rx,
            timing(),
            |result| assert!(result.is_ok()),
            move |error| *failed_in.lock().unwrap() = Some(error),
        );
    });
    tx.send(Signal::Fault(StreamFault::Transient)).unwrap();
    tx.send(Signal::Fault(StreamFault::Fatal)).unwrap();
    // Keep the rebuilt stream delivering audio so it is not seen as stalled.
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(10));
        if let Some(heartbeat) = &*heartbeat_out.lock().unwrap() {
            heartbeat.fetch_add(1, Ordering::Relaxed);
        }
    }
    tx.send(Signal::Stop).unwrap();
    supervisor.join().unwrap();
    assert_eq!(*opens.lock().unwrap(), 2);
    assert_eq!(*failed.lock().unwrap(), None);
}

#[test]
fn a_stalled_stream_that_cannot_reopen_reports_failure() {
    let opens = Arc::new(Mutex::new(0));
    let streams = FakeStreams {
        opens: opens.clone(),
        fail_reopens: true,
    };
    let (_tx, rx) = mpsc::channel();
    let failed = Arc::new(Mutex::new(None));
    let failed_in = failed.clone();
    supervise(
        |heartbeat| streams.open(heartbeat),
        rx,
        timing(),
        |result| assert!(result.is_ok()),
        move |error| *failed_in.lock().unwrap() = Some(error),
    );
    // The first open, then one reopen per allowed rebuild.
    assert_eq!(*opens.lock().unwrap(), 3);
    let failed = failed.lock().unwrap().clone().expect("recovery gave up");
    assert!(failed.contains("device gone"), "{failed}");
}

#[test]
fn a_failed_first_open_is_reported_to_the_caller() {
    let (_tx, rx) = mpsc::channel();
    let mut started = None;
    supervise(
        |_| anyhow::Result::<()>::Err(anyhow::anyhow!("no microphone")),
        rx,
        timing(),
        |result| started = Some(result.is_err()),
        |_| panic!("not a recovery failure"),
    );
    assert_eq!(started, Some(true));
}
