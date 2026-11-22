//! Screen-aware correction of voice transcripts: builds a vocabulary from what a pane shows and
//! keeps the corrections log that pins and suppresses mappings.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use local_transcription::context_correction::{
    self, Correction, CorrectionLog, DEFAULT_THRESHOLD, LogEntry, Vocabulary, identifier_tokens,
};
use warp_core::features::FeatureFlag;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use crate::settings::AISettings;

const LOG_FILE_NAME: &str = "voice_corrections.json";
/// Directory entries taken from the current directory.
const MAX_DIRECTORY_ENTRIES: usize = 300;
/// Branches taken from the repository.
const MAX_BRANCHES: usize = 100;
/// Recent history commands read.
const MAX_HISTORY_COMMANDS: usize = 300;
/// Bytes of screen text and agent reply scanned for identifiers.
const MAX_SCREEN_BYTES: usize = 16 * 1024;
/// Corrections that can still be reverted.
const MAX_PENDING: usize = 20;

/// Whether transcripts should be corrected.
pub fn is_enabled(app: &AppContext) -> bool {
    FeatureFlag::VoiceInput.is_enabled() && *AISettings::as_ref(app).voice_context_correction
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn log_path() -> PathBuf {
    warp_core::paths::data_dir().join(LOG_FILE_NAME)
}

/// What a pane shows, gathered on the UI thread without I/O.
#[derive(Debug, Clone, Default)]
pub struct PaneContext {
    pub cwd: Option<PathBuf>,
    /// Recent commands, newest first.
    pub history: Vec<String>,
    /// The latest block output and agent reply.
    pub screen_text: String,
}

impl PaneContext {
    /// Keeps the tail of `text`, where the newest output is, within the scan limit.
    pub fn push_screen_text(&mut self, text: &str) {
        let start = text.len().saturating_sub(MAX_SCREEN_BYTES);
        let start = (start..text.len())
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(text.len());
        self.screen_text.push('\n');
        self.screen_text.push_str(&text[start..]);
    }
}

/// Builds a vocabulary for `pane`. Does blocking I/O (directory listing, `.git` files, `PATH`);
/// call it off the UI thread.
pub fn build_vocabulary(pane: &PaneContext) -> Vocabulary {
    let mut vocabulary = Vocabulary::default();
    for command in pane.history.iter().take(MAX_HISTORY_COMMANDS) {
        let mut words = command.split_whitespace();
        if let Some(first) = words.next() {
            vocabulary.add(first);
            if let Some(second) = words.next().filter(|w| !w.starts_with('-')) {
                vocabulary.add(&format!("{first} {second}"));
            }
        }
        vocabulary.extend(identifier_tokens(command));
    }
    vocabulary.extend(identifier_tokens(&pane.screen_text));
    if let Some(cwd) = &pane.cwd {
        if let Ok(entries) = std::fs::read_dir(cwd) {
            for entry in entries.flatten().take(MAX_DIRECTORY_ENTRIES) {
                if let Some(name) = entry.file_name().to_str() {
                    vocabulary.add(name);
                }
            }
        }
        for term in git_terms(cwd) {
            vocabulary.add(&term);
        }
    }
    for executable in path_executables().iter() {
        if !vocabulary.add(executable)
            && vocabulary.len() >= context_correction::MAX_VOCABULARY_TERMS
        {
            break;
        }
    }
    vocabulary
}

/// The current branch, local branches, and remotes of the repository containing `cwd`, read
/// from `.git` without running git.
fn git_terms(cwd: &Path) -> Vec<String> {
    let Some(git_dir) = cwd
        .ancestors()
        .map(|dir| dir.join(".git"))
        .find(|p| p.is_dir())
    else {
        return Vec::new();
    };
    let mut terms = Vec::new();
    if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD"))
        && let Some(branch) = head.trim().strip_prefix("ref: refs/heads/")
    {
        terms.push(branch.to_owned());
    }
    let heads = git_dir.join("refs/heads");
    let mut stack = vec![heads.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(name) = path.strip_prefix(&heads) {
                terms.push(name.to_string_lossy().into_owned());
            }
            if terms.len() >= MAX_BRANCHES {
                return terms;
            }
        }
    }
    if let Ok(config) = std::fs::read_to_string(git_dir.join("config")) {
        terms.extend(config.lines().filter_map(|line| {
            line.trim()
                .strip_prefix("[remote \"")
                .and_then(|rest| rest.strip_suffix("\"]"))
                .map(str::to_owned)
        }));
    }
    terms
}

/// Executable names on `PATH`, read once per run.
fn path_executables() -> &'static [String] {
    static EXECUTABLES: OnceLock<Vec<String>> = OnceLock::new();
    EXECUTABLES.get_or_init(|| {
        let mut seen = HashSet::new();
        let mut names = Vec::new();
        let path = std::env::var_os("PATH").unwrap_or_default();
        for dir in std::env::split_paths(&path) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str()
                    && !name.starts_with('.')
                    && seen.insert(name.to_owned())
                {
                    names.push(name.to_owned());
                }
            }
        }
        names
    })
}

/// A correction the user can still revert from the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCorrection {
    pub id: u64,
    pub heard: String,
    pub corrected: String,
}

pub enum VoiceCorrectionsEvent {
    /// The log changed; the settings list refreshes.
    LogChanged,
    /// The user asked to revert `0` in the input that holds it.
    Revert(PendingCorrection),
}

/// The corrections log, persisted as JSON in the app data dir, and the corrections that can
/// still be reverted.
#[derive(Default)]
pub struct VoiceCorrections {
    log: CorrectionLog,
    pending: Vec<PendingCorrection>,
    next_id: u64,
}

impl VoiceCorrections {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.spawn(
            async { std::fs::read_to_string(log_path()).ok() },
            |me, json, ctx| {
                let Some(json) = json else {
                    return;
                };
                match CorrectionLog::from_json(&json) {
                    Ok(log) => {
                        me.log = log;
                        ctx.emit(VoiceCorrectionsEvent::LogChanged);
                    }
                    Err(error) => log::warn!("Ignoring unreadable voice corrections log: {error}"),
                }
            },
        );
        Self::default()
    }

    pub fn entries(&self) -> &[LogEntry] {
        self.log.entries()
    }

    /// Corrects `text` towards `vocabulary`, logging each correction. Corrections from earlier
    /// utterances that weren't reverted count as accepted.
    pub fn correct(
        &mut self,
        text: &str,
        vocabulary: &Vocabulary,
        ctx: &mut ModelContext<Self>,
    ) -> (String, Vec<(Correction, u64)>) {
        self.accept_pending(ctx);
        let result = context_correction::correct(text, vocabulary, &self.log, DEFAULT_THRESHOLD);
        if result.corrections.is_empty() {
            return (result.text, Vec::new());
        }
        let now = now();
        let mut corrections = Vec::new();
        for correction in result.corrections {
            self.log
                .record_correction(&correction.heard, &correction.corrected, now);
            self.next_id += 1;
            self.pending.push(PendingCorrection {
                id: self.next_id,
                heard: correction.heard.clone(),
                corrected: correction.corrected.clone(),
            });
            corrections.push((correction, self.next_id));
        }
        let excess = self.pending.len().saturating_sub(MAX_PENDING);
        self.pending.drain(..excess);
        self.changed(ctx);
        (result.text, corrections)
    }

    /// Counts every correction that can still be reverted as accepted.
    pub fn accept_pending(&mut self, ctx: &mut ModelContext<Self>) {
        if self.pending.is_empty() {
            return;
        }
        let now = now();
        for pending in std::mem::take(&mut self.pending) {
            self.log
                .record_accepted(&pending.heard, &pending.corrected, now);
        }
        self.changed(ctx);
    }

    /// Reverts correction `id`: logs it as reverted and asks its input to restore the heard words.
    pub fn revert(&mut self, id: u64, ctx: &mut ModelContext<Self>) {
        let Some(index) = self.pending.iter().position(|p| p.id == id) else {
            return;
        };
        let pending = self.pending.remove(index);
        self.log
            .record_reverted(&pending.heard, &pending.corrected, now());
        ctx.emit(VoiceCorrectionsEvent::Revert(pending));
        self.changed(ctx);
    }

    pub fn toggle_pinned(&mut self, heard: &str, corrected: &str, ctx: &mut ModelContext<Self>) {
        if self.log.pinned_target(heard) == Some(corrected) {
            self.log.unpin(heard, corrected);
        } else {
            self.log.pin(heard, corrected, now());
        }
        self.changed(ctx);
    }

    pub fn toggle_disabled(&mut self, heard: &str, corrected: &str, ctx: &mut ModelContext<Self>) {
        let disabled = self
            .log
            .entries()
            .iter()
            .any(|e| e.heard == heard && e.corrected == corrected && e.disabled);
        self.log.set_disabled(heard, corrected, !disabled, now());
        self.changed(ctx);
    }

    pub fn remove(&mut self, heard: &str, corrected: &str, ctx: &mut ModelContext<Self>) {
        self.log.remove(heard, corrected);
        self.changed(ctx);
    }

    /// Writes the log as JSON to `path` off the UI thread.
    pub fn export(&self, path: PathBuf, ctx: &mut ModelContext<Self>) {
        let json = self.log.to_json();
        ctx.background_executor()
            .spawn(async move {
                if let Err(error) = std::fs::write(&path, json) {
                    log::warn!("Failed to export voice corrections: {error}");
                }
            })
            .detach();
    }

    fn changed(&mut self, ctx: &mut ModelContext<Self>) {
        let json = self.log.to_json();
        ctx.background_executor()
            .spawn(async move {
                let path = log_path();
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(error) = std::fs::write(&path, json) {
                    log::warn!("Failed to save voice corrections: {error}");
                }
            })
            .detach();
        ctx.emit(VoiceCorrectionsEvent::LogChanged);
        ctx.notify();
    }
}

impl Entity for VoiceCorrections {
    type Event = VoiceCorrectionsEvent;
}

impl SingletonEntity for VoiceCorrections {}

/// A vocabulary shared by an input's transcripts until the pane context changes.
pub type SharedVocabulary = Arc<Vocabulary>;

#[cfg(test)]
#[path = "context_correction_tests.rs"]
mod tests;
