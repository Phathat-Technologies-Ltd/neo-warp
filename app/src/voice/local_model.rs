//! The on-device speech model that answers voice input in organization builds, where the embedded
//! server (`crates/selfhost_server`) serves `/ai/transcribe`. The model downloads only after the
//! user agrees, from the prompt shown when they first record or from settings.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use futures::channel::mpsc;
use local_transcription::{
    DownloadError, DownloadProgress, ModelAction, ModelBundle, ModelEvent, ModelSpec, ModelState,
    ModelStore, ProgressThrottle, campplus_en_voxceleb, kokoro_82m_v1_int8, parakeet_tdt_0_6b_v3,
    silero_vad_v5, smart_turn_v3_2, tiny_voca,
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

/// The store holding the voice activity detector that hands-free input segments speech with.
pub fn vad_model_store() -> ModelStore {
    ModelStore::new(warp_core::paths::data_dir().join("models"), silero_vad_v5())
}

/// The store holding the turn detector that tells a finished request from a mid-thought pause.
pub fn turn_model_store() -> ModelStore {
    ModelStore::new(
        warp_core::paths::data_dir().join("models"),
        smart_turn_v3_2(),
    )
}

/// Whether hands-free voice input is available, which also needs the VAD model.
pub fn is_hands_free_enabled() -> bool {
    is_local_voice_enabled() && FeatureFlag::VoiceHandsFree.is_enabled()
}

/// The models voice input needs: the speech model, plus the VAD and turn detector when hands-free
/// is on, so one consent covers them all.
fn model_bundle() -> ModelBundle {
    let mut stores = vec![model_store()];
    if FeatureFlag::VoiceHandsFree.is_enabled() {
        stores.push(vad_model_store());
        stores.push(turn_model_store());
    }
    ModelBundle::new(stores)
}

/// The on-device engines that read agent replies aloud in conversation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechOutputEngine {
    Kokoro,
    TinyVoca,
}

impl SpeechOutputEngine {
    /// Parses the `conversation_speech_engine` setting, defaulting to Kokoro.
    pub fn from_setting(value: &str) -> Self {
        match value {
            "tiny_voca" => SpeechOutputEngine::TinyVoca,
            _ => SpeechOutputEngine::Kokoro,
        }
    }

    pub fn spec(self) -> ModelSpec {
        match self {
            SpeechOutputEngine::Kokoro => kokoro_82m_v1_int8(),
            SpeechOutputEngine::TinyVoca => tiny_voca(),
        }
    }
}

/// The store holding the speech output model for `engine`.
pub fn speech_output_store(engine: SpeechOutputEngine) -> ModelStore {
    ModelStore::new(warp_core::paths::data_dir().join("models"), engine.spec())
}

/// The store holding the speaker embedding model that recognizes the user's voice.
pub fn speaker_model_store() -> ModelStore {
    ModelStore::new(
        warp_core::paths::data_dir().join("models"),
        campplus_en_voxceleb(),
    )
}

/// Whether "Recognize my voice" is available for hands-free and conversation mode.
pub fn is_speaker_id_enabled() -> bool {
    is_hands_free_enabled() && FeatureFlag::VoiceSpeakerId.is_enabled()
}

#[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
/// Whether voice conversation mode is available.
pub fn is_conversation_enabled() -> bool {
    is_hands_free_enabled() && FeatureFlag::VoiceConversation.is_enabled()
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

/// What differs between the downloadable voice models: their toasts and download action.
pub trait VoiceModelKind: 'static {
    const TOAST_ID: &'static str;
    /// Names the model in toasts, e.g. "speech model".
    const NOUN: &'static str;
    fn download_action() -> WorkspaceAction;
    fn cancel_action() -> WorkspaceAction;
    fn consent_prompt(display_name: &str, size: u64) -> String;
}

/// The speech recognition models voice input transcribes with.
pub struct SpeechInputKind;

impl VoiceModelKind for SpeechInputKind {
    const TOAST_ID: &'static str = "local-voice-model";
    const NOUN: &'static str = "speech model";

    fn download_action() -> WorkspaceAction {
        WorkspaceAction::DownloadLocalVoiceModel
    }

    fn cancel_action() -> WorkspaceAction {
        WorkspaceAction::CancelVoiceModelDownload {
            speech_output: false,
        }
    }

    fn consent_prompt(display_name: &str, size: u64) -> String {
        consent_prompt(display_name, size)
    }
}

/// The text-to-speech model conversation mode reads replies with.
pub struct SpeechOutputKind;

impl VoiceModelKind for SpeechOutputKind {
    const TOAST_ID: &'static str = "speech-output-model";
    const NOUN: &'static str = "speech output model";

    fn download_action() -> WorkspaceAction {
        WorkspaceAction::DownloadSpeechOutputModel
    }

    fn cancel_action() -> WorkspaceAction {
        WorkspaceAction::CancelVoiceModelDownload {
            speech_output: true,
        }
    }

    fn consent_prompt(display_name: &str, size: u64) -> String {
        format!(
            "Conversation mode reads replies aloud on this computer with {display_name}. Download \
             the model files ({})? Nothing is downloaded until you choose to.",
            format_megabytes(size)
        )
    }
}

/// The speaker embedding model that recognizes the user's voice.
pub struct SpeakerIdKind;

impl VoiceModelKind for SpeakerIdKind {
    const TOAST_ID: &'static str = "speaker-id-model";
    const NOUN: &'static str = "speaker model";

    fn download_action() -> WorkspaceAction {
        WorkspaceAction::DownloadSpeakerModel
    }

    fn cancel_action() -> WorkspaceAction {
        WorkspaceAction::CancelSpeakerModelDownload
    }

    fn consent_prompt(display_name: &str, size: u64) -> String {
        format!(
            "Recognizing your voice runs on this computer with {display_name}. Download the model \
             file ({})? Nothing is downloaded until you choose to, and your voiceprint never \
             leaves this computer.",
            format_megabytes(size)
        )
    }
}

pub type LocalVoiceModel = VoiceModel<SpeechInputKind>;
pub type SpeechOutputModel = VoiceModel<SpeechOutputKind>;
pub type SpeakerIdModel = VoiceModel<SpeakerIdKind>;

enum DownloadUpdate {
    Progress(DownloadProgress),
    Done(Result<(), String>),
}

/// A downloadable voice model, fetched only after the user agrees.
pub struct VoiceModel<K: VoiceModelKind> {
    store: ModelBundle,
    state: ModelState,
    /// Cancels the running download; replaced on each download so late updates from a cancelled
    /// one are recognized and dropped.
    cancel: Option<Arc<AtomicBool>>,
    /// The thread running the latest download, joined before deleting files it may still write.
    download_thread: Option<std::thread::JoinHandle<()>>,
    /// The window showing the download's progress toast.
    toast_window: Option<WindowId>,
    kind: PhantomData<K>,
}

/// How the speech model toast changes after a state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToastUpdate {
    ShowProgress(DownloadProgress),
    UpdateProgress(DownloadProgress),
    Ready,
    Failed(String),
    Dismiss,
}

fn toast_update(before: &ModelState, after: &ModelState) -> Option<ToastUpdate> {
    match (before, after) {
        (ModelState::Downloading(_), ModelState::Downloading(progress)) => {
            Some(ToastUpdate::UpdateProgress(*progress))
        }
        (_, ModelState::Downloading(progress)) => Some(ToastUpdate::ShowProgress(*progress)),
        (ModelState::Downloading(_), ModelState::Installed) => Some(ToastUpdate::Ready),
        (ModelState::Downloading(_), ModelState::Failed(message)) => {
            Some(ToastUpdate::Failed(message.clone()))
        }
        (ModelState::Downloading(_), ModelState::NotInstalled | ModelState::AwaitingConsent) => {
            Some(ToastUpdate::Dismiss)
        }
        (
            ModelState::NotInstalled
            | ModelState::AwaitingConsent
            | ModelState::Installed
            | ModelState::Failed(_),
            ModelState::NotInstalled
            | ModelState::AwaitingConsent
            | ModelState::Installed
            | ModelState::Failed(_),
        ) => None,
    }
}

impl LocalVoiceModel {
    pub fn new() -> Self {
        Self::with_bundle(model_bundle())
    }
}

impl SpeakerIdModel {
    pub fn new() -> Self {
        Self::with_bundle(ModelBundle::new(vec![speaker_model_store()]))
    }

    /// Whether the model files are on disk.
    pub fn is_installed(&self) -> bool {
        self.store.is_installed()
    }
}

impl SpeechOutputModel {
    pub fn new(engine: SpeechOutputEngine) -> Self {
        Self::with_bundle(ModelBundle::new(vec![speech_output_store(engine)]))
    }

    #[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
    /// Switches to `engine`'s model, cancelling a download of the previous one.
    pub fn set_engine(&mut self, engine: SpeechOutputEngine, ctx: &mut ModelContext<Self>) {
        let bundle = ModelBundle::new(vec![speech_output_store(engine)]);
        if bundle.display_name() == self.store.display_name() {
            return;
        }
        self.stop_download();
        if let Some(window_id) = self.toast_window.take() {
            remove_toast::<SpeechOutputKind>(window_id, ctx);
        }
        self.state = ModelState::from_status(bundle.status());
        self.store = bundle;
        ctx.emit(LocalVoiceModelEvent::StateChanged);
        ctx.notify();
    }
}

impl<K: VoiceModelKind> VoiceModel<K> {
    fn with_bundle(store: ModelBundle) -> Self {
        let state = ModelState::from_status(store.status());
        Self {
            store,
            state,
            cancel: None,
            download_thread: None,
            toast_window: None,
            kind: PhantomData,
        }
    }

    pub fn state(&self) -> &ModelState {
        &self.state
    }

    pub fn download_size(&self) -> u64 {
        self.store.total_size()
    }

    pub fn display_name(&self) -> String {
        self.store.display_name()
    }

    #[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
    /// Records the user's attempt to record and says whether recording can go ahead.
    pub fn request_recording(&mut self, ctx: &mut ModelContext<Self>) -> RecordingReadiness {
        if self.state.is_installed() && !self.store.is_installed() {
            // The files were deleted outside the app.
            self.apply(ModelEvent::Loaded(self.store.status()), ctx);
        }
        let download_ended = self
            .download_thread
            .as_ref()
            .is_some_and(|thread| thread.is_finished());
        if matches!(self.state, ModelState::Downloading(_))
            && download_ended
            && self.store.is_installed()
        {
            // The thread finished but its completion update hasn't arrived; trust the disk.
            self.cancel = None;
            self.apply(ModelEvent::DownloadFinished, ctx);
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

    /// Starts the download, showing its progress in a toast in `window_id`.
    pub fn download(&mut self, window_id: WindowId, ctx: &mut ModelContext<Self>) {
        self.toast_window = Some(window_id);
        self.apply(ModelEvent::DownloadRequested, ctx);
    }

    /// Shows the running download's progress toast in `window_id`.
    #[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
    fn show_progress_toast(&mut self, window_id: WindowId, ctx: &mut ModelContext<Self>) {
        if let ModelState::Downloading(progress) = self.state {
            if let Some(previous) = self.toast_window.replace(window_id) {
                if previous != window_id {
                    remove_toast::<K>(previous, ctx);
                }
            }
            add_toast::<K>(progress_toast::<K>(progress), window_id, ctx);
        }
    }

    fn sync_toast(&mut self, before: &ModelState, ctx: &mut ModelContext<Self>) {
        let Some(window_id) = self.toast_window else {
            return;
        };
        let Some(update) = toast_update(before, &self.state) else {
            return;
        };
        match update {
            ToastUpdate::ShowProgress(progress) => {
                add_toast::<K>(progress_toast::<K>(progress), window_id, ctx);
            }
            ToastUpdate::UpdateProgress(progress) => {
                ToastStack::handle(ctx).update(ctx, |stack, ctx| {
                    stack.update_toast_in_place(progress_toast::<K>(progress), window_id, ctx);
                });
            }
            ToastUpdate::Ready => {
                self.toast_window = None;
                remove_toast::<K>(window_id, ctx);
                let message = format!("{} ready", capitalize(K::NOUN));
                ToastStack::handle(ctx).update(ctx, |stack, ctx| {
                    stack.add_ephemeral_toast(DismissibleToast::success(message), window_id, ctx);
                });
            }
            ToastUpdate::Failed(message) => {
                let toast =
                    DismissibleToast::error(format!("The {} download failed: {message}", K::NOUN))
                        .with_link(
                            ToastLink::new("Retry".to_owned())
                                .with_onclick_action(K::download_action()),
                        )
                        .with_object_id(K::TOAST_ID.to_owned());
                add_toast::<K>(toast, window_id, ctx);
            }
            ToastUpdate::Dismiss => {
                self.toast_window = None;
                remove_toast::<K>(window_id, ctx);
            }
        }
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
            self.sync_toast(&before, ctx);
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

/// How often progress reaches the UI while the whole percent stays the same.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);

/// Runs the download on the calling thread with its own runtime, reporting progress as
/// [`ProgressThrottle`] allows.
fn download_blocking(
    store: &ModelBundle,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(DownloadProgress),
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let client = reqwest::Client::new();
    let mut throttle = ProgressThrottle::new(PROGRESS_INTERVAL);
    let result = runtime.block_on(store.download(&client, cancel, |progress| {
        if throttle.should_report(progress, Instant::now()) {
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

impl<K: VoiceModelKind> Entity for VoiceModel<K> {
    type Event = LocalVoiceModelEvent;
}

impl<K: VoiceModelKind> SingletonEntity for VoiceModel<K> {}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

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
    ensure_model_ready::<SpeechInputKind>(window_id, ctx)
}

/// Checks that the speaker model is installed before enrollment, asking for consent to download
/// it when it isn't.
#[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
pub fn ensure_speaker_model_ready(window_id: WindowId, ctx: &mut AppContext) -> bool {
    ensure_model_ready::<SpeakerIdKind>(window_id, ctx)
}

/// Checks that the speech output model is installed before conversation mode starts, asking for
/// consent to download it when it isn't.
#[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
pub fn ensure_speech_output_ready(window_id: WindowId, ctx: &mut AppContext) -> bool {
    ensure_model_ready::<SpeechOutputKind>(window_id, ctx)
}

#[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
fn ensure_model_ready<K: VoiceModelKind>(window_id: WindowId, ctx: &mut AppContext) -> bool {
    let model = VoiceModel::<K>::handle(ctx);
    let readiness = model.update(ctx, |model, ctx| model.request_recording(ctx));
    let toast = match readiness {
        RecordingReadiness::Ready => return true,
        RecordingReadiness::NeedsConsent => {
            let model = model.as_ref(ctx);
            DismissibleToast::default(K::consent_prompt(
                &model.display_name(),
                model.download_size(),
            ))
            .with_link(
                ToastLink::new("Download".to_owned()).with_onclick_action(K::download_action()),
            )
        }
        RecordingReadiness::Downloading(_) => {
            model.update(ctx, |model, ctx| model.show_progress_toast(window_id, ctx));
            return false;
        }
    }
    .with_object_id(K::TOAST_ID.to_owned());
    add_toast::<K>(toast, window_id, ctx);
    false
}

/// Shows `toast` in `window_id` as the only toast for this model there.
fn add_toast<K: VoiceModelKind>(
    toast: DismissibleToast<WorkspaceAction>,
    window_id: WindowId,
    ctx: &mut AppContext,
) {
    ToastStack::handle(ctx).update(ctx, |stack, ctx| {
        stack.add_persistent_toast(toast, window_id, ctx);
    });
}

fn remove_toast<K: VoiceModelKind>(window_id: WindowId, ctx: &mut AppContext) {
    ToastStack::handle(ctx).update(ctx, |stack, ctx| {
        stack.remove_toast_by_identifier(K::TOAST_ID.to_owned(), window_id, ctx);
    });
}

fn progress_toast<K: VoiceModelKind>(
    progress: DownloadProgress,
) -> DismissibleToast<WorkspaceAction> {
    // The cancel link stops the download; the model's files stay deletable from settings.
    DismissibleToast::default(progress_message(K::NOUN, progress))
        .with_link(ToastLink::new("Cancel".to_owned()).with_onclick_action(K::cancel_action()))
        .with_object_id(K::TOAST_ID.to_owned())
}

fn progress_message(noun: &str, progress: DownloadProgress) -> String {
    format!("Downloading the {noun}: {}", format_progress(progress))
}

#[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
fn consent_prompt(display_name: &str, size: u64) -> String {
    format!(
        "Voice input is transcribed on this computer by {display_name}. Download the model files ({})? \
         Nothing is downloaded until you choose to.",
        format_megabytes(size)
    )
}

/// Formats download progress for the UI, e.g. "42% (282 MB of 671 MB)".
pub fn format_progress(progress: DownloadProgress) -> String {
    match progress.percent() {
        None => "starting".to_owned(),
        Some(percent) => format!(
            "{percent}% ({} of {})",
            format_megabytes(progress.downloaded),
            format_megabytes(progress.total)
        ),
    }
}

#[cfg(test)]
#[path = "local_model_tests.rs"]
mod tests;
