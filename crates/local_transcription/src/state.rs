use crate::model::{DownloadProgress, ModelStatus};

/// What the UI knows about a local model, from the user's point of view. Nothing downloads
/// without an explicit user action: accepting the prompt shown when they first record, or the
/// download button in settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelState {
    NotInstalled,
    /// The user tried to record without the model; the UI is asking whether to download it.
    AwaitingConsent,
    /// The user declined the prompt. Recording doesn't ask again until the app restarts; settings
    /// still offers the download.
    Declined,
    Downloading(DownloadProgress),
    Installed,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelEvent {
    /// The model store was (re)read from disk.
    Loaded(ModelStatus),
    RecordingRequested,
    ConsentGiven,
    ConsentDeclined,
    /// The user pressed download, in settings or on the prompt.
    DownloadRequested,
    Progress(DownloadProgress),
    DownloadFinished,
    DownloadFailed(String),
    CancelRequested,
    RemoveRequested,
}

/// A side effect the owner of the state must carry out after a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelAction {
    AskConsent,
    StartDownload,
    CancelDownload,
    /// Stop any running download, then delete the model's files.
    DeleteFiles,
}

impl ModelState {
    pub fn from_status(status: ModelStatus) -> Self {
        match status {
            ModelStatus::Installed => Self::Installed,
            ModelStatus::NotInstalled | ModelStatus::Partial { .. } => Self::NotInstalled,
        }
    }

    pub fn is_installed(&self) -> bool {
        matches!(self, Self::Installed)
    }

    /// Applies `event` and returns the effect to carry out, if any.
    pub fn apply(&mut self, event: ModelEvent) -> Option<ModelAction> {
        use ModelAction::*;
        use ModelEvent::*;
        let (next, action) = match (&*self, event) {
            (Self::Downloading(_), Loaded(_)) => return None,
            (_, Loaded(status)) => (Self::from_status(status), None),

            // The prompt may have been dismissed without an answer, so ask again.
            (Self::NotInstalled | Self::AwaitingConsent | Self::Failed(_), RecordingRequested) => {
                (Self::AwaitingConsent, Some(AskConsent))
            }
            (Self::Declined | Self::Downloading(_) | Self::Installed, RecordingRequested) => {
                return None;
            }

            (Self::AwaitingConsent, ConsentGiven) => (
                Self::Downloading(DownloadProgress {
                    downloaded: 0,
                    total: 0,
                }),
                Some(StartDownload),
            ),
            (Self::AwaitingConsent, ConsentDeclined) => (Self::Declined, None),
            (_, ConsentGiven | ConsentDeclined) => return None,

            (
                Self::NotInstalled | Self::AwaitingConsent | Self::Declined | Self::Failed(_),
                DownloadRequested,
            ) => (
                Self::Downloading(DownloadProgress {
                    downloaded: 0,
                    total: 0,
                }),
                Some(StartDownload),
            ),
            (Self::Downloading(_) | Self::Installed, DownloadRequested) => {
                return None;
            }

            (Self::Downloading(_), Progress(progress)) => (Self::Downloading(progress), None),
            (Self::Downloading(_), DownloadFinished) => (Self::Installed, None),
            (Self::Downloading(_), DownloadFailed(message)) => (Self::Failed(message), None),
            (Self::Downloading(_), CancelRequested) => (Self::NotInstalled, Some(CancelDownload)),
            (_, Progress(_) | DownloadFinished | DownloadFailed(_) | CancelRequested) => {
                return None;
            }

            (_, RemoveRequested) => (Self::NotInstalled, Some(DeleteFiles)),
        };
        *self = next;
        action
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
