//! Naming, listing, renaming, and deleting recording files.

use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, TimeZone as _};

use crate::asciicast::{self, FILE_EXTENSION};

const MAX_TITLE_LEN: usize = 60;

#[derive(Clone, Debug, PartialEq)]
pub struct RecordingInfo {
    pub path: PathBuf,
    pub title: String,
    pub started_at: Option<DateTime<Local>>,
    /// Seconds.
    pub duration: f64,
    pub size_bytes: u64,
}

/// Turns a pane title into a file-name-safe slug.
pub fn sanitize_title(title: &str) -> String {
    let mut slug = String::new();
    for ch in title.chars() {
        if ch.is_alphanumeric() || ch == '.' || ch == '_' {
            slug.push(ch);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug
        .trim_matches(['-', '.'])
        .chars()
        .take(MAX_TITLE_LEN)
        .collect();
    let slug = slug.trim_end_matches(['-', '.']).to_owned();
    if slug.is_empty() {
        "session".to_owned()
    } else {
        slug
    }
}

pub fn recording_file_name(started_at: DateTime<Local>, title: &str) -> String {
    format!(
        "{}-{}.{FILE_EXTENSION}",
        started_at.format("%Y-%m-%d_%H-%M-%S"),
        sanitize_title(title)
    )
}

/// A path in `dir` for a new recording that doesn't collide with an existing file.
pub fn new_recording_path(dir: &Path, started_at: DateTime<Local>, title: &str) -> PathBuf {
    let file_name = recording_file_name(started_at, title);
    let candidate = dir.join(&file_name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = file_name.trim_end_matches(&format!(".{FILE_EXTENSION}"));
    (2..)
        .map(|n| dir.join(format!("{stem}-{n}.{FILE_EXTENSION}")))
        .find(|path| !path.exists())
        .unwrap_or(candidate)
}

/// Every readable `.cast` file directly in `dir`, newest first. A missing directory is empty.
pub fn list_recordings(dir: &Path) -> io::Result<Vec<RecordingInfo>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut recordings: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == FILE_EXTENSION))
        .filter_map(|path| match read_info(&path) {
            Ok(info) => Some(info),
            Err(error) => {
                log::warn!(
                    "Skipping unreadable recording {}: {error:#}",
                    path.display()
                );
                None
            }
        })
        .collect();
    recordings.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| b.path.cmp(&a.path))
    });
    Ok(recordings)
}

pub fn read_info(path: &Path) -> anyhow::Result<RecordingInfo> {
    let file = File::open(path)?;
    let size_bytes = file.metadata()?.len();
    let (header, duration) = asciicast::read_summary(BufReader::new(file))?;
    let title = header.title.clone().unwrap_or_else(|| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    Ok(RecordingInfo {
        path: path.to_owned(),
        title,
        started_at: header
            .timestamp
            .and_then(|secs| Local.timestamp_opt(secs, 0).single()),
        duration,
        size_bytes,
    })
}

/// Renames the file to `new_name` (sanitized, `.cast` kept) in the same directory, refusing to
/// overwrite another file. Returns the new path.
pub fn rename_recording(path: &Path, new_name: &str) -> io::Result<PathBuf> {
    let new_name = new_name.trim();
    let new_name = new_name
        .strip_suffix(&format!(".{FILE_EXTENSION}"))
        .unwrap_or(new_name);
    let target = path.with_file_name(format!("{}.{FILE_EXTENSION}", sanitize_title(new_name)));
    if target == path {
        return Ok(target);
    }
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", target.display()),
        ));
    }
    fs::rename(path, &target)?;
    Ok(target)
}

pub fn delete_recording(path: &Path) -> io::Result<()> {
    fs::remove_file(path)
}

/// `1:05`, `12:00`, or `1:02:03`.
pub fn format_duration(seconds: f64) -> String {
    let total = seconds.max(0.).round() as u64;
    let (hours, minutes, secs) = (total / 3600, (total / 60) % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.;
    let bytes_f = bytes as f64;
    if bytes_f < KB {
        format!("{bytes} B")
    } else if bytes_f < KB * KB {
        format!("{:.1} KB", bytes_f / KB)
    } else {
        format!("{:.1} MB", bytes_f / (KB * KB))
    }
}

#[cfg(test)]
#[path = "naming_tests.rs"]
mod tests;
