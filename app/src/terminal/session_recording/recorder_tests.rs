use std::fs::File;
use std::io::BufReader;
use std::time::Duration;

use super::*;
use crate::terminal::session_recording::asciicast::{self, EventData};

fn output_only() -> RecordingOptions {
    RecordingOptions {
        record_input: false,
        extra_redact_patterns: Vec::new(),
    }
}

#[test]
fn records_output_and_resizes_with_increasing_times() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("test.cast");
    let (tx, rx) = async_broadcast::broadcast(16);

    let recording =
        ActiveRecording::start(path.clone(), Header::new(80, 24), rx, output_only()).unwrap();
    tx.try_broadcast(Arc::new(b"first ".to_vec())).unwrap();
    wait_until_read(&tx);
    std::thread::sleep(Duration::from_millis(30));
    recording.resize(100, 40);
    std::thread::sleep(Duration::from_millis(30));
    tx.try_broadcast(Arc::new(b"second\n".to_vec())).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(recording.stop().unwrap(), path);

    let cast = asciicast::parse(BufReader::new(File::open(&path).unwrap())).unwrap();
    let data: Vec<_> = cast.events.iter().map(|event| event.data.clone()).collect();
    assert_eq!(
        data,
        vec![
            EventData::Output("first ".to_owned()),
            EventData::Resize {
                cols: 100,
                rows: 40
            },
            EventData::Output("second\n".to_owned()),
        ]
    );
    let times: Vec<_> = cast.events.iter().map(|event| event.time).collect();
    assert!(times.windows(2).all(|pair| pair[0] < pair[1]), "{times:?}");
    assert!(times[2] >= 0.05, "{times:?}");
}

#[test]
fn dropping_flushes_and_refuses_to_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.cast");
    let (tx, rx) = async_broadcast::broadcast(16);
    let recording =
        ActiveRecording::start(path.clone(), Header::new(10, 5), rx.clone(), output_only())
            .unwrap();
    tx.try_broadcast(Arc::new(b"x\n".to_vec())).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    drop(recording);

    let cast = asciicast::parse(BufReader::new(File::open(&path).unwrap())).unwrap();
    assert_eq!(cast.events.len(), 1);
    assert!(ActiveRecording::start(path, Header::new(10, 5), rx, output_only()).is_err());
}

fn record(
    options: RecordingOptions,
    steps: impl FnOnce(&async_broadcast::Sender<Arc<Vec<u8>>>, &ActiveRecording),
) -> Vec<EventData> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.cast");
    let (tx, rx) = async_broadcast::broadcast(16);
    let recording = ActiveRecording::start(path.clone(), Header::new(80, 24), rx, options).unwrap();
    steps(&tx, &recording);
    recording.stop().unwrap();
    asciicast::parse(BufReader::new(File::open(&path).unwrap()))
        .unwrap()
        .events
        .into_iter()
        .map(|event| event.data)
        .collect()
}

fn pause() {
    std::thread::sleep(Duration::from_millis(20));
}

fn wait_until_read(tx: &async_broadcast::Sender<Arc<Vec<u8>>>) {
    while !tx.is_empty() {
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn redacts_output_and_skips_input_by_default() {
    let events = record(output_only(), |tx, recording| {
        tx.try_broadcast(Arc::new(b"export TOKEN=abc123\n".to_vec()))
            .unwrap();
        pause();
        recording.input(b"typed\r");
        pause();
    });
    assert_eq!(
        events,
        vec![EventData::Output("export TOKEN=[REDACTED]\n".to_owned())]
    );
}

#[test]
fn records_input_but_not_passwords() {
    let options = RecordingOptions {
        record_input: true,
        extra_redact_patterns: vec!["corp-[0-9]+".to_owned()],
    };
    let events = record(options, |tx, recording| {
        recording.input(b"sudo ls corp-42\r");
        pause();
        tx.try_broadcast(Arc::new(b"[sudo] password for amit: ".to_vec()))
            .unwrap();
        wait_until_read(tx);
        pause();
        recording.input(b"hunter2");
        pause();
        recording.input(b"\r");
        pause();
        recording.suppress_input_until_next_line();
        pause();
        recording.input(b"s3cret\rexit\r");
        pause();
    });
    let input: String = events
        .iter()
        .filter_map(|event| match event {
            EventData::Input(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(input, "sudo ls [REDACTED]\r\r\rexit\r");
}
