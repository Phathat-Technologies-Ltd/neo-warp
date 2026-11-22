//! The on-device speech model that answers voice input in organization builds, where the embedded
//! server (`crates/selfhost_server`) serves `/ai/transcribe`. The model downloads only after the
//! user agrees, from the prompt shown when they first record or from settings.

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::channel::mpsc;
use local_transcription::{
    DownloadError, DownloadProgress, ModelAction, ModelEvent, ModelState, ModelStore,
    parakeet_tdt_0_6b_v3,
};
use warp_core::features::FeatureFlag;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity, WindowId};

use crate::view_components::{DismissibleToast, ToastLink};
use crate::workspace::{ToastStack, WorkspaceAction};

/// Set once the embedded server is started with the local transcriber.
static SERVED_LOCALLY: OnceLock<()> = OnceLock::new();

/// The store holding the speech model, under the app's data directory.
pub fn model_store() -> ModelStore {
    ModelStore::new(
        warp_core::paths::data_dir().join("models"),
        parakeet_tdt_0_6b_v3(),
    )
}

pub fn mark_served_locally() {
    let _ = SERVED_LOCALLY.set(());
}

/// Whether voice input is transcribed on this machine, so the model UI should show.
pub fn is_local_voice_enabled() -> bool {
    FeatureFlag::VoiceInput.is_enabled() && SERVED_LOCALLY.get().is_some()
}

/// What starting a recording needs first.
#[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingReadiness {
    Ready,
    /// Ask the user to download the model.
    NeedsConsent,
    Downloading(DownloadProgress),
}

pub enum LocalVoiceModelEvent {
    StateChanged,
}

enum DownloadUpdate {
    Progress(DownloadProgress),
    Done(Result<(), String>),
}

pub struct LocalVoiceModel {
    store: ModelStore,
    state: ModelState,
    /// Cancels the running download; replaced on each download so late updates from a cancelled
    /// one are recognized and dropped.
    cancel: Option<Arc<AtomicBool>>,
    /// The thread running the latest download, joined before deleting files it may still write.
    download_thread: Option<std::thread::JoinHandle<()>>,
}

impl LocalVoiceModel {
    pub fn new() -> Self {
        let store = model_store();
        let state = ModelState::from_status(store.status());
        Self {
            store,
            state,
            cancel: None,
            download_thread: None,
        }
    }

    pub fn state(&self) -> &ModelState {
        &self.state
    }

    pub fn download_size(&self) -> u64 {
        self.store.spec().total_size()
    }

    pub fn display_name(&self) -> &str {
        &self.store.spec().display_name
    }

    #[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
    /// Records the user's attempt to record and says whether recording can go ahead.
    pub fn request_recording(&mut self, ctx: &mut ModelContext<Self>) -> RecordingReadiness {
        if self.state.is_installed() && !self.store.is_installed() {
            // The files were deleted outside the app.
            self.apply(ModelEvent::Loaded(self.store.status()), ctx);
        }
        let asked =
            self.apply(ModelEvent::RecordingRequested, ctx) == Some(ModelAction::AskConsent);
        match &self.state {
            ModelState::Installed => RecordingReadiness::Ready,
            ModelState::Downloading(progress) => RecordingReadiness::Downloading(*progress),
            ModelState::NotInstalled | ModelState::AwaitingConsent | ModelState::Failed(_) => {
                debug_assert!(asked);
                RecordingReadiness::NeedsConsent
            }
        }
    }

    pub fn download(&mut self, ctx: &mut ModelContext<Self>) {
        self.apply(ModelEvent::DownloadRequested, ctx);
    }

    pub fn cancel_download(&mut self, ctx: &mut ModelContext<Self>) {
        self.apply(ModelEvent::CancelRequested, ctx);
    }

    pub fn remove(&mut self, ctx: &mut ModelContext<Self>) {
        self.apply(ModelEvent::RemoveRequested, ctx);
    }

    fn apply(&mut self, event: ModelEvent, ctx: &mut ModelContext<Self>) -> Option<ModelAction> {
        let before = self.state.clone();
        let action = self.state.apply(event);
        match action {
            Some(ModelAction::StartDownload) => self.start_download(ctx),
            Some(ModelAction::CancelDownload) => self.stop_download(),
            Some(ModelAction::DeleteFiles) => {
                self.stop_download();
                let download_thread = self.download_thread.take();
                let store = self.store.clone();
                // The cancelled download may still be writing, so wait for it to stop before
                // deleting; both happen off the main thread to keep the UI responsive.
                ctx.spawn(
                    async move {
                        if let Some(thread) = download_thread {
                            let _ = thread.join();
                        }
                        store.remove()
                    },
                    |_, result, _| {
                        if let Err(error) = result {
                            log::error!("Couldn't delete the voice model: {error}");
                        }
                    },
                );
            }
            Some(ModelAction::AskConsent) | None => {}
        }
        if self.state != before {
            ctx.emit(LocalVoiceModelEvent::StateChanged);
            ctx.notify();
        }
        action
    }

    fn stop_download(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    fn start_download(&mut self, ctx: &mut ModelContext<Self>) {
        self.stop_download();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        let (tx, rx) = mpsc::unbounded();
        let store = self.store.clone();
        let thread_cancel = cancel.clone();
        let spawned = std::thread::Builder::new()
            .name("voice-model-download".to_owned())
            .spawn(move || {
                let result = download_blocking(&store, &thread_cancel, |progress| {
                    let _ = tx.unbounded_send(DownloadUpdate::Progress(progress));
                });
                let _ = tx.unbounded_send(DownloadUpdate::Done(result));
            });
        match spawned {
            Ok(thread) => self.download_thread = Some(thread),
            Err(error) => {
                self.cancel = None;
                self.apply(ModelEvent::DownloadFailed(error.to_string()), ctx);
                return;
            }
        }
        ctx.spawn_stream_local(
            rx,
            move |me, update, ctx| {
                let is_current = me
                    .cancel
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &cancel));
                if !is_current {
                    return;
                }
                match update {
                    DownloadUpdate::Progress(progress) => {
                        me.apply(ModelEvent::Progress(progress), ctx);
                    }
                    DownloadUpdate::Done(result) => {
                        me.cancel = None;
                        let event = match result {
                            Ok(()) => ModelEvent::DownloadFinished,
                            Err(message) => ModelEvent::DownloadFailed(message),
                        };
                        me.apply(event, ctx);
                    }
                }
            },
            |_, _| {},
        );
    }
}

/// Runs the download on the calling thread with its own runtime, reporting progress at most once
/// per tenth of a percent.
fn download_blocking(
    store: &ModelStore,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(DownloadProgress),
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let client = reqwest::Client::new();
    let mut last_permille = None;
    let result = runtime.block_on(store.download(&client, cancel, |progress| {
        let permille = progress.downloaded * 1000 / progress.total.max(1);
        if last_permille != Some(permille) {
            last_permille = Some(permille);
            on_progress(progress);
        }
    }));
    match result {
        Ok(()) => Ok(()),
        Err(DownloadError::Cancelled) => Err("cancelled".to_owned()),
        Err(error) => {
            log::warn!("Voice model download failed: {error}");
            Err(error.to_string())
        }
    }
}

/// Formats a byte count as megabytes for the UI.
pub fn format_megabytes(bytes: u64) -> String {
    format!("{} MB", bytes.div_ceil(1_000_000))
}

impl Default for LocalVoiceModel {
    fn default() -> Self {
        Self::new()
    }
}

impl Entity for LocalVoiceModel {
    type Event = LocalVoiceModelEvent;
}

impl SingletonEntity for LocalVoiceModel {}

/// Whether the local model UI should show and, if so, the model's state.
pub fn local_model_state(app: &AppContext) -> Option<ModelState> {
    is_local_voice_enabled().then(|| LocalVoiceModel::as_ref(app).state().clone())
}

/// Checks that the local model can transcribe before a recording starts. When it can't, tells the
/// user why in a toast (offering the download when it isn't installed) and returns
/// `false`. Always `true` when voice isn't transcribed locally.
#[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
pub fn ensure_ready_to_record(window_id: WindowId, ctx: &mut AppContext) -> bool {
    if !is_local_voice_enabled() {
        return true;
    }
    let model = LocalVoiceModel::handle(ctx);
    let readiness = model.update(ctx, |model, ctx| model.request_recording(ctx));
    let toast = match readiness {
        RecordingReadiness::Ready => return true,
        RecordingReadiness::NeedsConsent => {
            let model = model.as_ref(ctx);
            DismissibleToast::default(consent_prompt(model.display_name(), model.download_size()))
                .with_link(
                    ToastLink::new("Download".to_owned())
                        .with_onclick_action(WorkspaceAction::DownloadLocalVoiceModel),
                )
        }
        RecordingReadiness::Downloading(progress) => DismissibleToast::default(format!(
            "The speech model is still downloading ({}).",
            format_progress(progress)
        )),
    }
    .with_object_id(TOAST_ID.to_owned());
    ToastStack::handle(ctx).update(ctx, |stack, ctx| {
        stack.remove_toast_by_identifier(TOAST_ID.to_owned(), window_id, ctx);
        stack.add_persistent_toast(toast, window_id, ctx);
    });
    false
}

const TOAST_ID: &str = "local-voice-model";

/// Removes the persistent speech model prompt from `window_id`, if it's showing.
pub fn dismiss_toast(window_id: WindowId, ctx: &mut AppContext) {
    ToastStack::handle(ctx).update(ctx, |stack, ctx| {
        stack.remove_toast_by_identifier(TOAST_ID.to_owned(), window_id, ctx);
    });
}

#[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
fn consent_prompt(display_name: &str, size: u64) -> String {
    format!(
        "Voice input is transcribed on this computer by {display_name}. Download the model ({})? \
         Nothing is downloaded until you choose to.",
        format_megabytes(size)
    )
}

/// Formats download progress as a percentage for the UI.
pub fn format_progress(progress: DownloadProgress) -> String {
    if progress.total == 0 {
        return "starting".to_owned();
    }
    format!("{}%", progress.downloaded * 100 / progress.total)
}

#[cfg(test)]
#[path = "local_model_tests.rs"]
mod tests;
