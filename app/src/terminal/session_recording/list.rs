//! State of the Recordings pane's list, independent of rendering.

use std::path::Path;

use chrono::{Local, TimeZone as _};
use session_recording::RecordingMetadata;
use warpui::elements::MouseStateHandle;

use super::naming::{format_duration, format_size};

#[derive(Default)]
pub struct RowMouseStates {
    pub play: MouseStateHandle,
    pub overflow: MouseStateHandle,
    pub confirm_delete: MouseStateHandle,
    pub cancel_delete: MouseStateHandle,
}

/// Recordings shorter or smaller than this hold no meaningful output.
const EMPTY_DURATION_SECONDS: f64 = 0.5;
const EMPTY_SIZE_BYTES: u64 = 256;

pub struct RecordingRow {
    pub info: RecordingMetadata,
    pub short_title: String,
    pub mouse_states: RowMouseStates,
}

impl RecordingRow {
    fn new(info: RecordingMetadata) -> Self {
        let short_title = short_title(&info.title, dirs::home_dir().as_deref());
        Self {
            info,
            short_title,
            mouse_states: Default::default(),
        }
    }

    pub fn is_empty_recording(&self) -> bool {
        !self.info.in_progress
            && (self.info.duration < EMPTY_DURATION_SECONDS
                || self.info.size_bytes < EMPTY_SIZE_BYTES)
    }

    /// Date, duration, and size, e.g. `2026-11-22 10:15  ·  1:05  ·  12 KB`.
    pub fn details(&self) -> String {
        let date = self
            .info
            .started_at
            .and_then(|secs| Local.timestamp_opt(secs, 0).single())
            .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        format!(
            "{date}  ·  {}  ·  {}",
            format_duration(self.info.duration),
            format_size(self.info.size_bytes)
        )
    }

    /// A recording still in progress keeps receiving events under its ID, so it can't be renamed.
    pub fn can_rename(&self) -> bool {
        !self.info.in_progress
    }
}

#[derive(Default)]
pub struct RecordingsList {
    rows: Vec<RecordingRow>,
    error: Option<String>,
    /// Index of the row whose name is being edited.
    renaming: Option<usize>,
    /// Index of the row awaiting delete confirmation.
    confirming_delete: Option<usize>,
}

impl RecordingsList {
    pub fn rows(&self) -> &[RecordingRow] {
        &self.rows
    }

    pub fn row(&self, index: usize) -> Option<&RecordingMetadata> {
        self.rows.get(index).map(|row| &row.info)
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn set_error(&mut self, message: String) {
        self.error = Some(message);
    }

    /// Replaces the list with a listing result, newest first. Pending edits are cancelled since
    /// row indices may have changed.
    pub fn set_listing(&mut self, result: anyhow::Result<Vec<RecordingMetadata>>) {
        match result {
            Ok(mut recordings) => {
                recordings.sort_by(|a, b| {
                    b.started_at
                        .cmp(&a.started_at)
                        .then_with(|| a.id.cmp(&b.id))
                });
                self.rows = recordings.into_iter().map(RecordingRow::new).collect();
                self.error = None;
            }
            Err(error) => {
                self.rows.clear();
                self.error = Some(format!("Couldn't list recordings: {error:#}"));
            }
        }
        self.renaming = None;
        self.confirming_delete = None;
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn renaming(&self) -> Option<usize> {
        self.renaming
    }

    /// Starts renaming a row, returning its current ID to prefill the editor.
    pub fn start_rename(&mut self, index: usize) -> Option<String> {
        let row = self.rows.get(index).filter(|row| row.can_rename())?;
        self.renaming = Some(index);
        self.confirming_delete = None;
        Some(row.info.id.clone())
    }

    pub fn cancel_rename(&mut self) {
        self.renaming = None;
    }

    /// Ends renaming, returning the ID of the row being renamed.
    pub fn finish_rename(&mut self) -> Option<String> {
        let index = self.renaming.take()?;
        self.row(index).map(|info| info.id.clone())
    }

    pub fn is_confirming_delete(&self, index: usize) -> bool {
        self.confirming_delete == Some(index)
    }

    pub fn cancel_delete(&mut self) {
        self.confirming_delete = None;
    }

    /// The first request for a row asks for confirmation; a second one returns its ID to delete.
    pub fn request_delete(&mut self, index: usize) -> Option<String> {
        let id = self.row(index)?.id.clone();
        if self.confirming_delete == Some(index) {
            self.confirming_delete = None;
            Some(id)
        } else {
            self.confirming_delete = Some(index);
            None
        }
    }
}

/// A recording title without repeated parts: titles join the pane title and its directory with
/// `": "`, and the pane title often already names that directory (`user@host:~/dir: /home/user/dir`).
/// Paths under `home` are shown relative to `~`, and a `user@host:` prompt prefix is dropped.
pub fn short_title(title: &str, home: Option<&Path>) -> String {
    let home = home.and_then(Path::to_str).filter(|home| !home.is_empty());
    let parts: Vec<String> = title
        .split(": ")
        .map(|part| tildify(strip_prompt_prefix(part.trim()), home))
        .filter(|part| !part.is_empty())
        .collect();
    let mut kept: Vec<&String> = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let repeated = parts.iter().enumerate().any(|(other_index, other)| {
            other_index != index
                && other.contains(part.as_str())
                && (other.len() > part.len() || other_index < index)
        });
        if !repeated {
            kept.push(part);
        }
    }
    if kept.is_empty() {
        return title.trim().to_owned();
    }
    kept.into_iter().cloned().collect::<Vec<_>>().join(" · ")
}

fn strip_prompt_prefix(part: &str) -> &str {
    match part.split_once(':') {
        Some((prefix, rest))
            if prefix.contains('@')
                && !prefix.contains(char::is_whitespace)
                && !rest.is_empty() =>
        {
            rest
        }
        _ => part,
    }
}

fn tildify(part: &str, home: Option<&str>) -> String {
    match home.and_then(|home| part.strip_prefix(home)) {
        Some("") => "~".to_owned(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => part.to_owned(),
    }
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
