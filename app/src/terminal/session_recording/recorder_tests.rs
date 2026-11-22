use std::io::Cursor;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use session_recording::asciicast::Header;

use super::*;
use crate::terminal::session_recording::asciicast::{self, EventData};

/// Stores appended batches like the server does (deduplicating resent ones), after failing the
/// first `failures` requests.
#[derive(Default)]
struct FakeClient {
    failures: AtomicUsize,
    always_fail: bool,
    started: Mutex<Option<CreateRecording>>,
    batches: Mutex<Vec<(u64, String)>>,
    finished: AtomicUsize,
}

impl FakeClient {
    fn failing_first(failures: usize) -> Self {
        Self {
            failures: AtomicUsize::new(failures),
            ..Default::default()
        }
    }

    fn fail(&self) -> anyhow::Result<()> {
        if self.always_fail
            || self
                .failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
        {
            Err(anyhow!("offline"))
        } else {
            Ok(())
        }
    }

    fn content(&self) -> String {
        let header = self.started.lock().unwrap().clone().unwrap().header;
        let mut content = serde_json::to_string(&header).unwrap();
        content.push('\n');
        for (_, batch) in self.batches.lock().unwrap().iter() {
            content.push_str(batch);
        }
        content
    }

    fn events(&self) -> Vec<EventData> {
        asciicast::parse(Cursor::new(self.content()))
            .unwrap()
            .events
            .into_iter()
            .map(|event| event.data)
            .collect()
    }
}

#[async_trait]
impl RecordingsClient for FakeClient {
    async fn start(&self, request: &CreateRecording) -> anyhow::Result<String> {
        self.fail()?;
        *self.started.lock().unwrap() = Some(request.clone());
        Ok("rec".to_owned())
    }

    async fn append(&self, id: &str, seq: u64, events: String) -> anyhow::Result<()> {
        assert_eq!(id, "rec");
        self.fail()?;
        let mut batches = self.batches.lock().unwrap();
        let expected = batches.len() as u64;
        assert!(seq <= expected, "batch {seq} sent before {expected}");
        if seq == expected {
            batches.push((seq, events));
        }
        Ok(())
    }

    async fn finish(&self, id: &str) -> anyhow::Result<RecordingMetadata> {
        self.fail()?;
        self.finished.fetch_add(1, Ordering::SeqCst);
        Ok(RecordingMetadata {
            id: id.to_owned(),
            title: "t".to_owned(),
            started_at: None,
            duration: 0.,
            size_bytes: 0,
            local_path: None,
        })
    }

    async fn list(&self) -> anyhow::Result<Vec<RecordingMetadata>> {
        unimplemented!()
    }

    async fn read(&self, _id: &str) -> anyhow::Result<Vec<u8>> {
        unimplemented!()
    }

    async fn rename(&self, _id: &str, _name: &str) -> anyhow::Result<RecordingMetadata> {
        unimplemented!()
    }

    async fn delete(&self, _id: &str) -> anyhow::Result<()> {
        unimplemented!()
    }
}

fn output_only() -> RecordingOptions {
    RecordingOptions {
        record_input: false,
        extra_redact_patterns: Vec::new(),
    }
}

fn fast_policy() -> UploadPolicy {
    UploadPolicy {
        max_batch_bytes: 64 * 1024,
        flush_interval: Duration::from_millis(5),
        retry_initial: Duration::from_millis(1),
        retry_max: Duration::from_millis(5),
        request_timeout: Duration::from_secs(5),
        finish_deadline: Duration::from_millis(500),
        max_buffered_bytes: 1024 * 1024,
    }
}

fn request(width: u16, height: u16) -> CreateRecording {
    CreateRecording {
        header: Header::new(width, height),
        name: "test".to_owned(),
    }
}

fn start(
    client: &Arc<FakeClient>,
    rx: Receiver<Arc<Vec<u8>>>,
    options: RecordingOptions,
    policy: UploadPolicy,
) -> ActiveRecording {
    ActiveRecording::start(client.clone(), request(80, 24), rx, options, policy).unwrap()
}

fn record_with(
    client: &Arc<FakeClient>,
    options: RecordingOptions,
    policy: UploadPolicy,
    steps: impl FnOnce(&async_broadcast::Sender<Arc<Vec<u8>>>, &ActiveRecording),
) -> anyhow::Result<RecordingMetadata> {
    let (tx, rx) = async_broadcast::broadcast(64);
    let recording = start(client, rx, options, policy);
    steps(&tx, &recording);
    future::block_on(recording.stop())
}

fn record(
    options: RecordingOptions,
    steps: impl FnOnce(&async_broadcast::Sender<Arc<Vec<u8>>>, &ActiveRecording),
) -> Vec<EventData> {
    let client = Arc::new(FakeClient::default());
    record_with(&client, options, fast_policy(), steps).unwrap();
    client.events()
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
fn records_output_and_resizes_with_increasing_times() {
    let client = Arc::new(FakeClient::default());
    record_with(&client, output_only(), fast_policy(), |tx, recording| {
        tx.try_broadcast(Arc::new(b"first ".to_vec())).unwrap();
        wait_until_read(tx);
        std::thread::sleep(Duration::from_millis(30));
        recording.resize(100, 40);
        std::thread::sleep(Duration::from_millis(30));
        tx.try_broadcast(Arc::new(b"second\n".to_vec())).unwrap();
        std::thread::sleep(Duration::from_millis(30));
    })
    .unwrap();

    let cast = asciicast::parse(Cursor::new(client.content())).unwrap();
    assert_eq!(cast.header.width, 80);
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
    assert_eq!(client.finished.load(Ordering::SeqCst), 1);
}

#[test]
fn sends_contiguous_batches_in_order() {
    let client = Arc::new(FakeClient::default());
    let policy = UploadPolicy {
        max_batch_bytes: 1,
        ..fast_policy()
    };
    record_with(&client, output_only(), policy, |tx, _| {
        for n in 0..20 {
            tx.try_broadcast(Arc::new(format!("line {n}\n").into_bytes()))
                .unwrap();
            wait_until_read(tx);
        }
    })
    .unwrap();
    let batches = client.batches.lock().unwrap();
    assert!(batches.len() >= 20, "{}", batches.len());
    assert!(
        batches
            .iter()
            .enumerate()
            .all(|(i, (seq, _))| *seq == i as u64)
    );
    drop(batches);
    let output: String = client
        .events()
        .into_iter()
        .map(|event| match event {
            EventData::Output(text) => text,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    let expected: String = (0..20).map(|n| format!("line {n}\n")).collect();
    assert_eq!(output, expected);
}

#[test]
fn retries_failed_requests_without_losing_events() {
    let client = Arc::new(FakeClient::failing_first(5));
    record_with(&client, output_only(), fast_policy(), |tx, _| {
        for chunk in ["one\n", "two\n", "three\n"] {
            tx.try_broadcast(Arc::new(chunk.as_bytes().to_vec()))
                .unwrap();
            pause();
        }
    })
    .unwrap();
    assert_eq!(
        client.events(),
        ["one\n", "two\n", "three\n"].map(|text| EventData::Output(text.to_owned()))
    );
    assert_eq!(client.finished.load(Ordering::SeqCst), 1);
}

#[test]
fn gives_up_after_the_finish_deadline() {
    let client = Arc::new(FakeClient {
        always_fail: true,
        ..Default::default()
    });
    let started = Instant::now();
    let result = record_with(&client, output_only(), fast_policy(), |tx, _| {
        tx.try_broadcast(Arc::new(b"lost\n".to_vec())).unwrap();
        pause();
    });
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn dropping_still_uploads() {
    let client = Arc::new(FakeClient::default());
    let (tx, rx) = async_broadcast::broadcast(16);
    let recording = start(&client, rx, output_only(), fast_policy());
    tx.try_broadcast(Arc::new(b"x\n".to_vec())).unwrap();
    pause();
    drop(recording);
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.finished.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(client.events(), [EventData::Output("x\n".to_owned())]);
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
