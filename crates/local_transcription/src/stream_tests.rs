use super::*;

/// Treats a frame as speech when its first sample is loud, so tests script speech by amplitude.
pub(crate) struct LoudnessVad;

impl VoiceActivityDetector for LoudnessVad {
    fn speech_probability(&mut self, frame: &[f32]) -> Result<f32, TranscribeError> {
        Ok(if frame[0].abs() > 0.25 { 0.9 } else { 0.0 })
    }

    fn reset(&mut self) {}
}

fn frames(amplitude: f32, count: usize) -> Vec<f32> {
    vec![amplitude; VAD_FRAME_SAMPLES * count]
}

fn stream() -> UtteranceStream {
    UtteranceStream::new(Box::new(LoudnessVad), StreamConfig::default())
}

fn messages(outputs: &[StreamOutput]) -> Vec<ServerMessage> {
    outputs
        .iter()
        .filter_map(|output| match output {
            StreamOutput::Message(message) => Some(message.clone()),
            StreamOutput::Decode(_) => None,
        })
        .collect()
}

fn decodes(outputs: &[StreamOutput]) -> Vec<&DecodeRequest> {
    outputs
        .iter()
        .filter_map(|output| match output {
            StreamOutput::Decode(request) => Some(request),
            StreamOutput::Message(_) => None,
        })
        .collect()
}

#[test]
fn segments_an_utterance_between_pauses() {
    let mut stream = stream();
    let mut outputs = stream.push_samples(&frames(0.0, 10)).unwrap();
    outputs.extend(stream.push_samples(&frames(0.5, 10)).unwrap());
    outputs.extend(stream.push_samples(&frames(0.0, 12)).unwrap());
    assert_eq!(
        messages(&outputs),
        vec![ServerMessage::SpeechStart, ServerMessage::SpeechEnd]
    );
    let decodes = decodes(&outputs);
    assert_eq!(decodes.len(), 1);
    let last = decodes[0];
    assert_eq!(last.kind, DecodeKind::Final);
    assert_eq!(last.utterance, 1);
    // Pre-roll (192 ms + the 2 start frames = 8 frames) plus speech plus trailing silence.
    let speech = last.audio.samples.iter().filter(|s| **s > 0.25).count();
    assert_eq!(speech, VAD_FRAME_SAMPLES * 10);
    assert!(last.audio.samples.len() > speech);
}

#[test]
fn requests_partials_every_interval() {
    let mut stream = stream();
    let outputs = stream.push_samples(&frames(0.5, 2 + 40)).unwrap();
    let partials: Vec<_> = decodes(&outputs)
        .into_iter()
        .filter(|request| request.kind == DecodeKind::Partial)
        .collect();
    // 40 frames = 1.28 s of audio after the start: two 600 ms intervals.
    assert_eq!(partials.len(), 2);
    assert!(partials[1].audio.samples.len() > partials[0].audio.samples.len());
}

#[test]
fn flush_ends_the_open_utterance() {
    let mut stream = stream();
    stream.push_samples(&frames(0.5, 5)).unwrap();
    let outputs = stream.flush();
    assert_eq!(messages(&outputs), vec![ServerMessage::SpeechEnd]);
    assert_eq!(decodes(&outputs)[0].kind, DecodeKind::Final);
    assert!(stream.flush().is_empty());
}

#[test]
fn utterance_ids_increase() {
    let mut stream = stream();
    let mut outputs = Vec::new();
    for _ in 0..2 {
        outputs.extend(stream.push_samples(&frames(0.5, 3)).unwrap());
        outputs.extend(stream.push_samples(&frames(0.0, 12)).unwrap());
    }
    let ids: Vec<_> = decodes(&outputs).iter().map(|r| r.utterance).collect();
    assert_eq!(ids, vec![1, 2]);
}

#[test]
fn decodes_pcm_split_at_odd_bytes() {
    let mut stream = stream();
    let sample = (0.5_f32 * 32768.0) as i16;
    let bytes: Vec<u8> = std::iter::repeat_n(sample.to_le_bytes(), VAD_FRAME_SAMPLES * 3)
        .flatten()
        .collect();
    let (first, second) = bytes.split_at(1001);
    let mut outputs = stream.push_pcm(first).unwrap();
    outputs.extend(stream.push_pcm(second).unwrap());
    assert_eq!(messages(&outputs), vec![ServerMessage::SpeechStart]);
}

#[test]
fn long_utterances_end_without_a_pause() {
    let mut stream = UtteranceStream::new(
        Box::new(LoudnessVad),
        StreamConfig {
            max_utterance_ms: 1000,
            ..StreamConfig::default()
        },
    );
    let outputs = stream.push_samples(&frames(0.5, 40)).unwrap();
    assert!(messages(&outputs).contains(&ServerMessage::SpeechEnd));
}

fn request(utterance: u64, kind: DecodeKind) -> DecodeRequest {
    DecodeRequest {
        utterance,
        kind,
        audio: Audio {
            samples: vec![],
            sample_rate: VAD_SAMPLE_RATE,
        },
    }
}

fn drain(queue: &mut DecodeQueue) -> Vec<(u64, DecodeKind)> {
    std::iter::from_fn(|| queue.pop())
        .map(|r| (r.utterance, r.kind))
        .collect()
}

#[test]
fn queue_keeps_only_the_newest_partial() {
    let mut queue = DecodeQueue::default();
    queue.push(request(1, DecodeKind::Partial));
    queue.push(request(1, DecodeKind::Partial));
    assert_eq!(drain(&mut queue), vec![(1, DecodeKind::Partial)]);
}

#[test]
fn queue_drops_partials_superseded_by_a_final() {
    let mut queue = DecodeQueue::default();
    queue.push(request(1, DecodeKind::Partial));
    queue.push(request(1, DecodeKind::Final));
    queue.push(request(1, DecodeKind::Partial));
    queue.push(request(2, DecodeKind::Partial));
    queue.push(request(2, DecodeKind::Final));
    assert_eq!(
        drain(&mut queue),
        vec![(1, DecodeKind::Final), (2, DecodeKind::Final)]
    );
}

#[test]
fn protocol_messages_match_the_spec() {
    let config: ClientMessage =
        serde_json::from_str(r#"{"type":"config","language":"en","vad":{"min_silence_ms":300}}"#)
            .unwrap();
    assert_eq!(
        config,
        ClientMessage::Config {
            language: Some("en".to_owned()),
            vad: Some(VadOptions {
                min_silence_ms: Some(300),
                smart_end_of_turn: None,
            }),
        }
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"type":"flush"}"#).unwrap(),
        ClientMessage::Flush
    );
    assert_eq!(
        serde_json::to_string(&ServerMessage::Partial {
            utterance: 3,
            text: "git".to_owned()
        })
        .unwrap(),
        r#"{"type":"partial","utterance":3,"text":"git"}"#
    );
    assert_eq!(
        serde_json::to_string(&ServerMessage::SpeechStart).unwrap(),
        r#"{"type":"speech_start"}"#
    );
}

/// Answers a fixed completion probability and counts its calls.
struct FakeTurnDetector {
    probability: Result<f32, ()>,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl TurnDetector for FakeTurnDetector {
    fn completion_probability(&mut self, _audio: &[f32]) -> Result<f32, TranscribeError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.probability
            .map_err(|()| TranscribeError::Engine("broken".to_owned()))
    }
}

fn smart_stream(
    probability: Result<f32, ()>,
) -> (
    UtteranceStream,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let calls = std::sync::Arc::default();
    let detector = FakeTurnDetector {
        probability,
        calls: std::sync::Arc::clone(&calls),
    };
    (stream().with_turn_detector(Box::new(detector)), calls)
}

/// Frames of silence after speech until the utterance ends, or `None` within `limit` frames.
fn silence_frames_until_end(stream: &mut UtteranceStream, limit: usize) -> Option<usize> {
    stream.push_samples(&frames(0.5, 10)).unwrap();
    (1..=limit).find(|_| {
        let outputs = stream.push_samples(&frames(0.0, 1)).unwrap();
        messages(&outputs).contains(&ServerMessage::SpeechEnd)
    })
}

#[test]
fn complete_turn_ends_at_the_probe() {
    let (mut stream, calls) = smart_stream(Ok(0.9));
    // 200 ms probe at 32 ms frames.
    assert_eq!(silence_frames_until_end(&mut stream, 100), Some(7));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn incomplete_turn_waits_for_the_long_timeout() {
    let (mut stream, calls) = smart_stream(Ok(0.1));
    // 2000 ms at 32 ms frames.
    assert_eq!(silence_frames_until_end(&mut stream, 100), Some(63));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn speech_after_an_incomplete_pause_continues_the_utterance() {
    let (mut stream, calls) = smart_stream(Ok(0.1));
    stream.push_samples(&frames(0.5, 10)).unwrap();
    let outputs = stream.push_samples(&frames(0.0, 20)).unwrap();
    assert!(!messages(&outputs).contains(&ServerMessage::SpeechEnd));
    assert_eq!(silence_frames_until_end(&mut stream, 100), Some(63));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn disabled_smart_end_of_turn_uses_the_fixed_timeout() {
    let (mut stream, calls) = smart_stream(Ok(0.1));
    stream.set_smart_end_of_turn(false);
    assert!(!stream.uses_turn_detector());
    // 300 ms at 32 ms frames.
    assert_eq!(silence_frames_until_end(&mut stream, 100), Some(10));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn failing_detector_falls_back_to_the_fixed_timeout() {
    let (mut stream, calls) = smart_stream(Err(()));
    assert_eq!(silence_frames_until_end(&mut stream, 100), Some(10));
    assert!(!stream.uses_turn_detector());
    assert_eq!(silence_frames_until_end(&mut stream, 100), Some(10));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn partial_ending_mid_thought_holds_the_turn_open() {
    let (mut stream, _) = smart_stream(Ok(0.9));
    stream.push_samples(&frames(0.5, 10)).unwrap();
    stream.note_partial(1, "list the files in");
    assert_eq!(silence_frames_until_end(&mut stream, 100), Some(63));
}

#[test]
fn partials_of_older_utterances_are_ignored() {
    let (mut stream, _) = smart_stream(Ok(0.9));
    stream.push_samples(&frames(0.5, 10)).unwrap();
    stream.note_partial(0, "list the files in");
    assert_eq!(silence_frames_until_end(&mut stream, 100), Some(7));
}
