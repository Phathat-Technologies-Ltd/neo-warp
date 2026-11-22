use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

#[test]
fn fills_frames_and_reports_the_end_once() {
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let mut shared = Shared {
        samples: VecDeque::from(vec![0.5, -0.5, 0.25]),
        on_finished: Some(Box::new(move || flag.store(true, Ordering::SeqCst))),
    };
    let mut out = [9.0; 4];
    assert!(shared.fill(&mut out, 2).is_none());
    assert_eq!(out, [0.5, 0.5, -0.5, -0.5]);
    let finished = shared.fill(&mut out, 2).expect("the clip ended");
    assert_eq!(out, [0.25, 0.25, 0.0, 0.0]);
    finished();
    assert!(done.load(Ordering::SeqCst));
    assert!(shared.fill(&mut out, 2).is_none());
}
