//! Reading and writing asciicast v2 files (<https://docs.asciinema.org/manual/asciicast/v2/>).

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};

use anyhow::{Context as _, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const FILE_EXTENSION: &str = "cast";

/// The first line of an asciicast v2 file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub version: u8,
    pub width: u16,
    pub height: u16,
    /// Unix time, in seconds, when the recording started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl Header {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            version: 2,
            width,
            height,
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EventData {
    Output(String),
    Input(String),
    Resize { cols: u16, rows: u16 },
    Marker(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Seconds since the start of the recording.
    pub time: f64,
    pub data: EventData,
}

/// A fully parsed recording.
#[derive(Clone, Debug, PartialEq)]
pub struct Cast {
    pub header: Header,
    pub events: Vec<Event>,
}

impl Cast {
    /// The recording's length: the header duration if present, otherwise the last event time.
    pub fn duration(&self) -> f64 {
        self.header
            .duration
            .or_else(|| self.events.last().map(|event| event.time))
            .unwrap_or(0.)
    }
}

/// Writes an asciicast v2 stream. Output bytes are decoded as UTF-8 incrementally, so a character
/// split across two PTY reads is written whole in the later event.
pub struct CastWriter<W: Write> {
    out: W,
    decoder: Utf8Decoder,
}

/// Decodes a byte stream as UTF-8, holding a trailing incomplete character until the next chunk
/// and replacing invalid sequences.
#[derive(Default)]
pub struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    pub fn decode(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        take_complete_utf8(&mut self.pending)
    }
}

impl<W: Write> CastWriter<W> {
    pub fn new(mut out: W, header: &Header) -> io::Result<Self> {
        serde_json::to_writer(&mut out, header)?;
        out.write_all(b"\n")?;
        Ok(Self {
            out,
            decoder: Utf8Decoder::default(),
        })
    }

    /// A writer for event lines only, for appending to a recording whose header was written
    /// elsewhere.
    pub fn events_only(out: W) -> Self {
        Self {
            out,
            decoder: Utf8Decoder::default(),
        }
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.out
    }

    pub fn write_output(&mut self, time: f64, bytes: &[u8]) -> io::Result<()> {
        let text = self.decoder.decode(bytes);
        self.write_output_text(time, &text)
    }

    pub fn write_output_text(&mut self, time: f64, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.write_event(time, "o", text)
    }

    pub fn write_input_text(&mut self, time: f64, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.write_event(time, "i", text)
    }

    pub fn write_resize(&mut self, time: f64, cols: u16, rows: u16) -> io::Result<()> {
        self.write_event(time, "r", &format!("{cols}x{rows}"))
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    pub fn into_inner(mut self) -> io::Result<W> {
        self.out.flush()?;
        Ok(self.out)
    }

    fn write_event(&mut self, time: f64, code: &str, data: &str) -> io::Result<()> {
        // Microsecond precision, like asciinema itself.
        let time = (time.max(0.) * 1_000_000.).round() / 1_000_000.;
        serde_json::to_writer(&mut self.out, &(time, code, data))?;
        self.out.write_all(b"\n")
    }
}

/// Removes and returns the longest valid UTF-8 prefix of `buffer`, replacing invalid sequences,
/// and leaves a trailing incomplete character in `buffer`.
fn take_complete_utf8(buffer: &mut Vec<u8>) -> String {
    let mut text = String::new();
    let mut rest = buffer.as_slice();
    loop {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                text.push_str(valid);
                rest = &[];
                break;
            }
            Err(error) => {
                let (valid, after) = rest.split_at(error.valid_up_to());
                text.push_str(std::str::from_utf8(valid).unwrap_or_default());
                match error.error_len() {
                    Some(invalid_len) => {
                        text.push(char::REPLACEMENT_CHARACTER);
                        rest = &after[invalid_len..];
                    }
                    None => {
                        rest = after;
                        break;
                    }
                }
            }
        }
    }
    let remainder = rest.to_vec();
    *buffer = remainder;
    text
}

/// Parses an asciicast v2 stream. Unknown event codes are skipped.
pub fn parse(reader: impl BufRead) -> anyhow::Result<Cast> {
    let mut lines = reader.lines();
    let header_line = lines.next().ok_or_else(|| anyhow!("empty recording"))??;
    let header: Header = serde_json::from_str(&header_line).context("invalid header")?;
    if header.version != 2 {
        bail!("unsupported asciicast version {}", header.version);
    }
    let mut events = Vec::new();
    for (index, line) in lines.enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(event) =
            parse_event(&line).with_context(|| format!("invalid event on line {}", index + 2))?
        {
            events.push(event);
        }
    }
    Ok(Cast { header, events })
}

fn parse_event(line: &str) -> anyhow::Result<Option<Event>> {
    let (time, code, data): (f64, String, Value) = serde_json::from_str(line)?;
    let data_str = || data.as_str().map(str::to_owned).unwrap_or_default();
    let data = match code.as_str() {
        "o" => EventData::Output(data_str()),
        "i" => EventData::Input(data_str()),
        "m" => EventData::Marker(data_str()),
        "r" => {
            let size = data_str();
            let (cols, rows) = size
                .split_once('x')
                .ok_or_else(|| anyhow!("invalid resize {size:?}"))?;
            EventData::Resize {
                cols: cols.trim().parse()?,
                rows: rows.trim().parse()?,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(Event { time, data }))
}

/// Checks that every non-empty line of `lines` is an asciicast v2 event (`[time, code, data]`).
pub fn validate_event_lines(lines: &str) -> anyhow::Result<()> {
    for (index, line) in lines.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        serde_json::from_str::<(f64, String, Value)>(line)
            .with_context(|| format!("invalid event on line {}", index + 1))?;
    }
    Ok(())
}

/// Reads only what a recordings list needs: the header and the last event time.
pub fn read_summary(reader: impl BufRead) -> anyhow::Result<(Header, f64)> {
    let mut lines = reader.lines();
    let header_line = lines.next().ok_or_else(|| anyhow!("empty recording"))??;
    let header: Header = serde_json::from_str(&header_line).context("invalid header")?;
    let mut last_time = 0.;
    for line in lines {
        let line = line?;
        if let Ok((time, ..)) = serde_json::from_str::<(f64, Value, Value)>(&line) {
            last_time = time;
        }
    }
    Ok((header.clone(), header.duration.unwrap_or(last_time)))
}

#[cfg(test)]
#[path = "asciicast_tests.rs"]
mod tests;
