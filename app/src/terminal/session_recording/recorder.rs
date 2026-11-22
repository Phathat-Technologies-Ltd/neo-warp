//! Writes one pane's PTY output (and, when enabled, keyboard input) to an asciicast file on a
//! dedicated thread, redacting secrets first.

use instant::Instant;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::thread::JoinHandle;

use async_broadcast::{Receiver, RecvError};
use futures_lite::future;
use regex::Regex;

use super::asciicast::{CastWriter, Header, Utf8Decoder};
use super::redact::Redactor;

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

enum Control {
    Resize {
        cols: u16,
        rows: u16,
    },
    Input(Vec<u8>),
    /// The terminal reported a password prompt (echo off), so skip input until the next line.
    SuppressInput,
    Stop,
}

enum Next {
    Output(Arc<Vec<u8>>),
    Control(Control),
    Done,
}

/// An in-progress recording. Dropping it stops the recording and flushes the file.
pub struct ActiveRecording {
    path: PathBuf,
    control_tx: async_channel::Sender<Control>,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl ActiveRecording {
    /// Creates the file and starts writing everything that arrives on `pty_reads`.
    pub fn start(
        path: PathBuf,
        header: Header,
        pty_reads: Receiver<Arc<Vec<u8>>>,
        options: RecordingOptions,
    ) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = File::create_new(&path)?;
        let writer = CastWriter::new(BufWriter::new(file), &header)?;
        let (control_tx, control_rx) = async_channel::unbounded();
        let thread = std::thread::Builder::new()
            .name("session-recording".to_owned())
            .spawn(move || {
                let state = StreamState::new(writer, options);
                future::block_on(state.run(Instant::now(), pty_reads, control_rx))
            })?;
        Ok(Self {
            path,
            control_tx,
            thread: Some(thread),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.control_tx.try_send(Control::Resize { cols, rows });
    }

    /// Records keyboard input. Ignored unless input recording was enabled at start.
    pub fn input(&self, bytes: &[u8]) {
        let _ = self.control_tx.try_send(Control::Input(bytes.to_vec()));
    }

    pub fn suppress_input_until_next_line(&self) {
        let _ = self.control_tx.try_send(Control::SuppressInput);
    }

    /// Stops recording and waits for the file to be flushed.
    pub fn stop(mut self) -> io::Result<PathBuf> {
        self.finish()?;
        Ok(self.path.clone())
    }

    fn finish(&mut self) -> io::Result<()> {
        let _ = self.control_tx.try_send(Control::Stop);
        match self.thread.take() {
            Some(thread) => thread
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("recording thread panicked"))),
            None => Ok(()),
        }
    }
}

impl Drop for ActiveRecording {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            log::warn!("Couldn't finish recording {}: {error}", self.path.display());
        }
    }
}

struct StreamState<W: Write> {
    writer: CastWriter<W>,
    record_input: bool,
    output_decoder: Utf8Decoder,
    output_redactor: Redactor,
    input_decoder: Utf8Decoder,
    input_redactor: Redactor,
    output_tail: String,
    is_input_suppressed: bool,
}

impl<W: Write> StreamState<W> {
    fn new(writer: CastWriter<W>, options: RecordingOptions) -> Self {
        let patterns = || options.extra_redact_patterns.iter().map(String::as_str);
        Self {
            writer,
            record_input: options.record_input,
            output_decoder: Utf8Decoder::default(),
            output_redactor: Redactor::new(patterns()),
            input_decoder: Utf8Decoder::default(),
            input_redactor: Redactor::new(patterns()),
            output_tail: String::new(),
            is_input_suppressed: false,
        }
    }

    async fn run(
        mut self,
        started_at: Instant,
        mut pty_reads: Receiver<Arc<Vec<u8>>>,
        control_rx: async_channel::Receiver<Control>,
    ) -> io::Result<()> {
        loop {
            let next = future::or(
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
            )
            .await;
            let time = started_at.elapsed().as_secs_f64();
            match next {
                Next::Output(bytes) => self.output(time, &bytes)?,
                Next::Control(Control::Resize { cols, rows }) => {
                    self.writer.write_resize(time, cols, rows)?
                }
                Next::Control(Control::Input(bytes)) => self.input(time, &bytes)?,
                Next::Control(Control::SuppressInput) => self.is_input_suppressed = true,
                Next::Control(Control::Stop) | Next::Done => break,
            }
            if pty_reads.is_empty() && control_rx.is_empty() {
                self.writer.flush()?;
            }
        }
        while let Ok(bytes) = pty_reads.try_recv() {
            self.output(started_at.elapsed().as_secs_f64(), &bytes)?;
        }
        let time = started_at.elapsed().as_secs_f64();
        let output = self.output_redactor.flush();
        self.writer.write_output_text(time, &output)?;
        if self.record_input {
            let input = self.input_redactor.flush();
            self.writer.write_input_text(time, &input)?;
        }
        self.writer.into_inner()?.flush()
    }

    fn output(&mut self, time: f64, bytes: &[u8]) -> io::Result<()> {
        let text = self.output_decoder.decode(bytes);
        if self.record_input {
            self.track_password_prompt(&text);
        }
        let text = self.output_redactor.push(&text);
        self.writer.write_output_text(time, &text)
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

    fn input(&mut self, time: f64, bytes: &[u8]) -> io::Result<()> {
        if !self.record_input {
            return Ok(());
        }
        let text = self.input_decoder.decode(bytes);
        let text = if self.is_input_suppressed {
            match text.find(['\r', '\n']) {
                Some(line_end) => {
                    self.is_input_suppressed = false;
                    self.output_tail.clear();
                    text[line_end..].to_owned()
                }
                None => return Ok(()),
            }
        } else {
            text
        };
        let text = self.input_redactor.push(&text);
        self.writer.write_input_text(time, &text)
    }
}

#[cfg(test)]
#[path = "recorder_tests.rs"]
mod tests;
