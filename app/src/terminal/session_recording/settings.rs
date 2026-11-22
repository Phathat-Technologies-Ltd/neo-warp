use std::path::PathBuf;

use settings::macros::define_settings_group;
use settings::{Setting as _, SupportedPlatforms, SyncToCloud};

define_settings_group!(SessionRecordingSettings, settings: [
    directory: SessionRecordingDirectory {
        type: String,
        default: String::new(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "session_recording.directory",
        description: "Directory where session recordings are saved. Empty uses the default.",
    },
    record_input: SessionRecordingRecordInput {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "session_recording.record_input",
        description: "Whether session recordings also capture keyboard input. Input is never recorded at password prompts.",
    },
    redact_patterns: SessionRecordingRedactPatterns {
        type: String,
        default: String::new(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "session_recording.redact_patterns",
        description: "Extra regular expressions, one per line, redacted from session recordings.",
    },
    idle_time_limit: SessionRecordingIdleTimeLimit {
        type: f32,
        default: 2.0,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "session_recording.idle_time_limit",
        description: "Pauses longer than this many seconds are shortened when playing a recording with Skip idle on.",
    },
]);

impl SessionRecordingSettings {
    /// Where new recordings are written and where the recordings list looks.
    pub fn recordings_dir(&self) -> PathBuf {
        resolve_recordings_dir(self.directory.value())
    }
}

impl SessionRecordingSettings {
    /// The idle time limit written to new recordings, in seconds.
    pub fn idle_time_limit(&self) -> f64 {
        let limit = f64::from(*self.idle_time_limit.value());
        if limit.is_finite() && limit > 0. {
            limit
        } else {
            2.
        }
    }

    /// The user's extra redaction patterns.
    pub fn user_redact_patterns(&self) -> Vec<String> {
        self.redact_patterns
            .value()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

pub fn resolve_recordings_dir(configured: &str) -> PathBuf {
    let configured = configured.trim();
    if configured.is_empty() {
        default_recordings_dir()
    } else {
        PathBuf::from(shellexpand::tilde(configured).into_owned())
    }
}

pub fn default_recordings_dir() -> PathBuf {
    warp_core::paths::data_dir().join("recordings")
}
