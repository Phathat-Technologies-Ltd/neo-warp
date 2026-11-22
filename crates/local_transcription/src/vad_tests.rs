use super::*;

fn run(segmenter: &mut Segmenter, probabilities: &[f32]) -> Vec<(usize, SegmentEvent)> {
    probabilities
        .iter()
        .enumerate()
        .filter_map(|(index, &p)| segmenter.push(p).map(|event| (index, event)))
        .collect()
}

#[test]
fn speech_needs_min_speech_frames() {
    let mut segmenter = Segmenter::new(SegmenterConfig::default());
    // A single loud frame, like a key press, doesn't start an utterance.
    assert!(run(&mut segmenter, &[0.9, 0.1, 0.1]).is_empty());
    assert_eq!(
        run(&mut segmenter, &[0.9, 0.9]),
        vec![(1, SegmentEvent::SpeechStart)]
    );
}

#[test]
fn short_pause_keeps_utterance_open() {
    let mut segmenter = Segmenter::new(SegmenterConfig::default());
    run(&mut segmenter, &[0.9, 0.9]);
    // 8 frames = 256 ms of silence: under 300 ms.
    let mut frames = vec![0.0; 8];
    frames.push(0.9);
    assert!(run(&mut segmenter, &frames).is_empty());
    assert!(segmenter.in_speech());
}

#[test]
fn long_pause_ends_utterance() {
    let mut segmenter = Segmenter::new(SegmenterConfig::default());
    run(&mut segmenter, &[0.9, 0.9]);
    // 300 ms / 32 ms rounds up to the 10th frame.
    assert_eq!(
        run(&mut segmenter, &[0.0; 12]),
        vec![(9, SegmentEvent::SpeechEnd)]
    );
    assert!(!segmenter.in_speech());
}

#[test]
fn uncertain_frames_continue_counting_silence_but_do_not_start_it() {
    let mut segmenter = Segmenter::new(SegmenterConfig::default());
    run(&mut segmenter, &[0.9, 0.9]);
    // Between the thresholds without prior silence: still speech.
    assert!(run(&mut segmenter, &[0.4; 20]).is_empty());
    // Once silence started, uncertain frames count toward it.
    let mut frames = vec![0.1];
    frames.extend([0.4; 9]);
    assert_eq!(
        run(&mut segmenter, &frames),
        vec![(9, SegmentEvent::SpeechEnd)]
    );
}

#[test]
fn min_silence_is_configurable() {
    let mut segmenter = Segmenter::new(SegmenterConfig::default());
    segmenter.set_min_silence_ms(640);
    run(&mut segmenter, &[0.9, 0.9]);
    assert_eq!(
        run(&mut segmenter, &[0.0; 25]),
        vec![(19, SegmentEvent::SpeechEnd)]
    );
}
