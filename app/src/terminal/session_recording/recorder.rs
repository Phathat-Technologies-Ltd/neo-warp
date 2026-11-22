//! Captures one pane's PTY output (and, when enabled, keyboard input) as asciicast events,
//! redacting secrets first, and streams them in batches to the recordings API.
//!
//! Capture and upload run on separate threads so the terminal never waits on the network: the
//! capture thread hands finished batches to the upload thread, which buffers them while the
//! server is unreachable and retries with backoff. Only redacted text leaves the capture thread.

use std::collections::VecDeque;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use anyhow::anyhow;
use async_broadcast::{Receiver, RecvError};
use async_channel::TryRecvError;
use futures_lite::future;
use instant::Instant;
use regex::Regex;
use session_recording::{CreateRecording, RecordingMetadata};
use warpui::r#async::Timer;

use super::asciicast::{CastWriter, Utf8Decoder};
use super::client::RecordingsClient;
use super::redact::Redactor;
use super::sanitize::WarpSequenceFilter;

/// Output ending like a password prompt; input after it is not recorded until the next line.
static PASSWORD_PROMPT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(password|passphrase|passcode|\bpin\b|verification code)[^\n]*[:?]\s*$")
        .expect("valid regex")
});
/// Output kept for password prompt detection, in bytes.
const PROMPT_TAIL_LEN: usize = 256;

pub struct RecordingOptions {
    pub record_input: bool,
    pub extra_redact_patterns: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct UploadPolicy {
    /// A batch is cut once it reaches this size, or when capture goes idle after
    /// `flush_interval`.
    pub max_batch_bytes: usize,
    pub flush_interval: Duration,
    pub retry_initial: Duration,
    pub retry_max: Duration,
    pub request_timeout: Duration,
    /// How long to keep retrying after the recording stops before giving up.
    pub finish_deadline: Duration,
    /// Unsent batches beyond this many bytes are dropped.
    pub max_buffered_bytes: usize,
}

impl Default for UploadPolicy {
    fn default() -> Self {
        Self {
            max_batch_bytes: 64 * 1024,
            flush_interval: Duration::from_millis(200),
            retry_initial: Duration::from_millis(250),
            retry_max: Duration::from_secs(10),
            request_timeout: Duration::from_secs(15),
            finish_deadline: Duration::from_secs(120),
            max_buffered_bytes: 256 * 1024 * 1024,
        }
    }
}

enum Control {
    Resize {
        cols: u16,
        rows: u16,
    },
    Input(Vec<u8>),
    /// A command started; the label is redacted before it's written.
    Marker(String),
    /// The terminal reported a password prompt (echo off), so skip input until the next line.
    SuppressInput,
    Stop,
}

enum Next {
    Output(Arc<Vec<u8>>),
    Control(Control),
    FlushDue,
    Done,
}

/// An in-progress recording. Dropping it stops capture; the upload finishes in the background.
pub struct ActiveRecording {
    control_tx: async_channel::Sender<Control>,
    result_rx: async_channel::Receiver<anyhow::Result<RecordingMetadata>>,
}

impl ActiveRecording {
    /// Starts capturing everything that arrives on `pty_reads`. The recording is created on the
    /// server by the upload thread, so this doesn't wait on the network.
    pub fn start(
        client: Arc<dyn RecordingsClient>,
        request: CreateRecording,
        pty_reads: Receiver<Arc<Vec<u8>>>,
        options: RecordingOptions,
        policy: UploadPolicy,
    ) -> std::io::Result<Self> {
        let (control_tx, control_rx) = async_channel::unbounded();
        let (batch_tx, batch_rx) = async_channel::unbounded();
        let (result_tx, result_rx) = async_channel::bounded(1);
        let upload_policy = policy.clone();
        std::thread::Builder::new()
            .name("session-recording-upload".to_owned())
            .spawn(move || {
                let result = future::block_on(upload(client, request, batch_rx, upload_policy));
                if let Err(error) = &result {
                    log::warn!("Couldn't save session recording: {error:#}");
                }
                let _ = result_tx.try_send(result);
            })?;
        std::thread::Builder::new()
            .name("session-recording".to_owned())
            .spawn(move || {
                let state = CaptureState::new(batch_tx, options, policy);
                future::block_on(state.run(Instant::now(), pty_reads, control_rx));
            })?;
        Ok(Self {
            control_tx,
            result_rx,
        })
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.control_tx.try_send(Control::Resize { cols, rows });
    }

    /// Records keyboard input. Ignored unless input recording was enabled at start.
    pub fn input(&self, bytes: &[u8]) {
        let _ = self.control_tx.try_send(Control::Input(bytes.to_vec()));
    }

    /// Writes a marker event, e.g. at the start of a command, so players can jump to it.
    pub fn marker(&self, label: &str) {
        let _ = self.control_tx.try_send(Control::Marker(label.to_owned()));
    }

    pub fn suppress_input_until_next_line(&self) {
        let _ = self.control_tx.try_send(Control::SuppressInput);
    }

    /// Stops capture. The returned future resolves once the recording is fully uploaded, or the
    /// upload gave up.
    pub fn stop(self) -> impl Future<Output = anyhow::Result<RecordingMetadata>> + Send + 'static {
        let _ = self.control_tx.try_send(Control::Stop);
        let result_rx = self.result_rx.clone();
        async move {
            result_rx
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow!("the recording upload stopped unexpectedly")))
        }
    }
}

impl Drop for ActiveRecording {
    fn drop(&mut self) {
        let _ = self.control_tx.try_send(Control::Stop);
    }
}

struct CaptureState {
    writer: CastWriter<Vec<u8>>,
    batch_tx: async_channel::Sender<String>,
    policy: UploadPolicy,
    last_batch_at: Instant,
    record_input: bool,
    output_decoder: Utf8Decoder,
    output_filter: WarpSequenceFilter,
    output_redactor: Redactor,
    input_decoder: Utf8Decoder,
    input_redactor: Redactor,
    marker_redactor: Redactor,
    output_tail: String,
    is_input_suppressed: bool,
}

impl CaptureState {
    fn new(
        batch_tx: async_channel::Sender<String>,
        options: RecordingOptions,
        policy: UploadPolicy,
    ) -> Self {
        let patterns = || options.extra_redact_patterns.iter().map(String::as_str);
        Self {
            writer: CastWriter::events_only(Vec::new()),
            batch_tx,
            policy,
            last_batch_at: Instant::now(),
            record_input: options.record_input,
            output_decoder: Utf8Decoder::default(),
            output_filter: WarpSequenceFilter::default(),
            output_redactor: Redactor::new(patterns()),
            input_decoder: Utf8Decoder::default(),
            input_redactor: Redactor::new(patterns()),
            marker_redactor: Redactor::new(patterns()),
            output_tail: String::new(),
            is_input_suppressed: false,
        }
    }

    async fn run(
        mut self,
        started_at: Instant,
        mut pty_reads: Receiver<Arc<Vec<u8>>>,
        control_rx: async_channel::Receiver<Control>,
    ) {
        loop {
            let flush_in = self.flush_in();
            let next = future::or(
                future::or(
                    async {
                        match control_rx.recv().await {
                            Ok(control) => Next::Control(control),
                            Err(_) => Next::Done,
                        }
                    },
                    async {
                        loop {
                            match pty_reads.recv().await {
                                Ok(bytes) => return Next::Output(bytes),
                                Err(RecvError::Overflowed(skipped)) => {
                                    log::warn!("Session recording dropped {skipped} PTY reads");
                                }
                                Err(RecvError::Closed) => return Next::Done,
                            }
                        }
                    },
                ),
                async {
                    match flush_in {
                        Some(delay) => {
                            Timer::after(delay).await;
                        }
                        None => future::pending::<()>().await,
                    }
                    Next::FlushDue
                },
            )
            .await;
            let time = started_at.elapsed().as_secs_f64();
            match next {
                Next::Output(bytes) => self.output(time, &bytes),
                Next::Control(Control::Resize { cols, rows }) => {
                    self.write(|writer| writer.write_resize(time, cols, rows))
                }
                Next::Control(Control::Input(bytes)) => self.input(time, &bytes),
                Next::Control(Control::Marker(label)) => self.marker(time, &label),
                Next::Control(Control::SuppressInput) => self.is_input_suppressed = true,
                Next::FlushDue => {}
                Next::Control(Control::Stop) | Next::Done => break,
            }
            let is_idle = pty_reads.is_empty() && control_rx.is_empty();
            if self.writer.get_mut().len() >= self.policy.max_batch_bytes
                || (is_idle && self.flush_in() == Some(Duration::ZERO))
            {
                self.cut_batch();
            }
        }
        while let Ok(bytes) = pty_reads.try_recv() {
            self.output(started_at.elapsed().as_secs_f64(), &bytes);
        }
        let time = started_at.elapsed().as_secs_f64();
        let held = self.output_filter.flush();
        let mut output = self.output_redactor.push(&held);
        output.push_str(&self.output_redactor.flush());
        self.write(|writer| writer.write_output_text(time, &output));
        if self.record_input {
            let input = self.input_redactor.flush();
            self.write(|writer| writer.write_input_text(time, &input));
        }
        self.cut_batch();
    }

    /// How long until buffered events should be sent; `None` when nothing is buffered.
    fn flush_in(&mut self) -> Option<Duration> {
        if self.writer.get_mut().is_empty() {
            return None;
        }
        Some(
            self.policy
                .flush_interval
                .saturating_sub(self.last_batch_at.elapsed()),
        )
    }

    fn cut_batch(&mut self) {
        let bytes = std::mem::take(self.writer.get_mut());
        self.last_batch_at = Instant::now();
        if bytes.is_empty() {
            return;
        }
        // Event lines are serialized from `str`s, so they are valid UTF-8.
        let batch = String::from_utf8_lossy(&bytes).into_owned();
        let _ = self.batch_tx.try_send(batch);
    }

    fn write(&mut self, write: impl FnOnce(&mut CastWriter<Vec<u8>>) -> std::io::Result<()>) {
        if let Err(error) = write(&mut self.writer) {
            log::warn!("Couldn't encode a session recording event: {error}");
        }
    }

    fn output(&mut self, time: f64, bytes: &[u8]) {
        let text = self.output_decoder.decode(bytes);
        let text = self.output_filter.push(&text);
        if self.record_input {
            self.track_password_prompt(&text);
        }
        let text = self.output_redactor.push(&text);
        self.write(|writer| writer.write_output_text(time, &text));
    }

    fn marker(&mut self, time: f64, label: &str) {
        let mut label = self.marker_redactor.push(label.trim());
        label.push_str(&self.marker_redactor.flush());
        self.write(|writer| writer.write_marker(time, &label));
    }

    fn track_password_prompt(&mut self, text: &str) {
        self.output_tail.push_str(text);
        if self.output_tail.len() > PROMPT_TAIL_LEN {
            let mut start = self.output_tail.len() - PROMPT_TAIL_LEN;
            while !self.output_tail.is_char_boundary(start) {
                start += 1;
            }
            self.output_tail.drain(..start);
        }
        if PASSWORD_PROMPT.is_match(&self.output_tail) {
            self.is_input_suppressed = true;
        }
    }

    fn input(&mut self, time: f64, bytes: &[u8]) {
        if !self.record_input {
            return;
        }
        let text = self.input_decoder.decode(bytes);
        let text = if self.is_input_suppressed {
            match text.find(['\r', '\n']) {
                Some(line_end) => {
                    self.is_input_suppressed = false;
                    self.output_tail.clear();
                    text[line_end..].to_owned()
                }
                None => return,
            }
        } else {
            text
        };
        let text = self.input_redactor.push(&text);
        self.write(|writer| writer.write_input_text(time, &text));
    }
}

/// Unsent batches, numbered in capture order.
struct Uploader {
    client: Arc<dyn RecordingsClient>,
    request: CreateRecording,
    policy: UploadPolicy,
    id: Option<String>,
    next_seq: u64,
    queue: VecDeque<(u64, String)>,
    queued_bytes: usize,
    has_dropped: bool,
}

impl Uploader {
    fn enqueue(&mut self, batch: String) {
        if self.queued_bytes + batch.len() > self.policy.max_buffered_bytes {
            if !self.has_dropped {
                log::warn!("Session recording upload is backed up; dropping events");
                self.has_dropped = true;
            }
            return;
        }
        self.queued_bytes += batch.len();
        self.queue.push_back((self.next_seq, batch));
        self.next_seq += 1;
    }

    /// Creates the recording if needed and sends every queued batch in order, stopping at the
    /// first failure.
    async fn send_queued(&mut self) -> anyhow::Result<()> {
        let id = match &self.id {
            Some(id) => id.clone(),
            None => {
                let id = with_timeout(
                    self.policy.request_timeout,
                    self.client.start(&self.request),
                )
                .await?;
                self.id = Some(id.clone());
                id
            }
        };
        while let Some((seq, batch)) = self.queue.front() {
            with_timeout(
                self.policy.request_timeout,
                self.client.append(&id, *seq, batch.clone()),
            )
            .await?;
            self.queued_bytes -= batch.len();
            self.queue.pop_front();
        }
        Ok(())
    }
}

async fn with_timeout<T>(
    timeout: Duration,
    request: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    future::or(request, async {
        Timer::after(timeout).await;
        Err(anyhow!("the recordings server didn't respond"))
    })
    .await
}

async fn upload(
    client: Arc<dyn RecordingsClient>,
    request: CreateRecording,
    batches: async_channel::Receiver<String>,
    policy: UploadPolicy,
) -> anyhow::Result<RecordingMetadata> {
    let mut uploader = Uploader {
        client,
        request,
        policy: policy.clone(),
        id: None,
        next_seq: 0,
        queue: VecDeque::new(),
        queued_bytes: 0,
        has_dropped: false,
    };
    let mut stopped_at: Option<Instant> = None;
    let mut backoff = policy.retry_initial;
    loop {
        loop {
            match batches.try_recv() {
                Ok(batch) => uploader.enqueue(batch),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => {
                    stopped_at.get_or_insert_with(Instant::now);
                    break;
                }
            }
        }
        match uploader.send_queued().await {
            Ok(()) => {
                backoff = policy.retry_initial;
                if stopped_at.is_some() {
                    break;
                }
                match batches.recv().await {
                    Ok(batch) => uploader.enqueue(batch),
                    Err(_) => {
                        stopped_at.get_or_insert_with(Instant::now);
                    }
                }
            }
            Err(error) => {
                if stopped_at.is_some_and(|at| at.elapsed() >= policy.finish_deadline) {
                    return Err(error);
                }
                log::debug!("Session recording upload failed, retrying: {error:#}");
                Timer::after(backoff).await;
                backoff = (backoff * 2).min(policy.retry_max);
            }
        }
    }
    let id = uploader
        .id
        .clone()
        .ok_or_else(|| anyhow!("the recording was never created"))?;
    loop {
        match with_timeout(policy.request_timeout, uploader.client.finish(&id)).await {
            Ok(metadata) => {
                if uploader.has_dropped {
                    log::warn!("Session recording {id} is missing events dropped while offline");
                }
                return Ok(metadata);
            }
            Err(error) => {
                if stopped_at.is_some_and(|at| at.elapsed() >= policy.finish_deadline) {
                    return Err(error);
                }
                Timer::after(backoff).await;
                backoff = (backoff * 2).min(policy.retry_max);
            }
        }
    }
}

#[cfg(test)]
#[path = "recorder_tests.rs"]
mod tests;
