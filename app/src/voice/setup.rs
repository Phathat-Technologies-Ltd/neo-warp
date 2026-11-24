//! Downloads every model a voice entry point needs after one consent, with one progress toast,
//! then starts what the user asked for.

use std::time::Duration;

use instant::Instant;

use local_transcription::{
    DownloadProgress, ModelBundle, ModelSpec, ModelState, ModelStatus, silero_vad_v5,
    smart_turn_v3_2,
};
use settings::Setting as _;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity, WindowId};

use super::local_model::{
    AcceleratedSpeechKind, AcceleratedSpeechModel, LocalVoiceModel, LocalVoiceModelEvent,
    SpeechInputKind, SpeechOutputEngine, SpeechOutputKind, SpeechOutputModel, VoiceModel,
    VoiceModelKind, format_gigabytes, format_megabytes, is_local_voice_enabled,
    offer_accelerated_speech_model, supports_neural_engine_package, transcription_acceleration,
};
use crate::settings::AISettings;
use crate::view_components::{DismissibleToast, ToastLink};
use crate::workspace::{ToastStack, WorkspaceAction};

const TOAST_ID: &str = "voice-setup";

/// How long after the click a finished download still starts the voice mode the user asked for.
const AUTO_START_WINDOW: Duration = Duration::from_secs(10 * 60);

/// A control that starts voice input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceEntry {
    PushToTalk,
    HandsFree,
    Conversation,
}

/// A downloadable voice model with its own settings row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceComponent {
    /// Speech recognition with the voice activity and turn detectors.
    SpeechInput,
    /// The speech encoder package for the Apple Neural Engine.
    NeuralEngine,
    /// The selected engine's text-to-speech model.
    SpeechOutput,
}

/// How much of a model is on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Missing,
    /// An interrupted download left some files.
    Partial,
    Installed,
}

impl From<ModelStatus> for Presence {
    fn from(status: ModelStatus) -> Self {
        match status {
            ModelStatus::NotInstalled => Presence::Missing,
            ModelStatus::Partial { .. } => Presence::Partial,
            ModelStatus::Installed => Presence::Installed,
        }
    }
}

/// What decides which models an entry point still needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Availability {
    pub speech_input: Presence,
    pub neural_engine: Presence,
    /// `None` when the selected engine downloads nothing.
    pub speech_output: Option<Presence>,
    /// Apple silicon on macOS 15 or later, with acceleration on Auto.
    pub neural_engine_supported: bool,
    /// The Neural Engine package was offered before (on its own or with the speech models).
    pub neural_engine_offered: bool,
}

/// The models `entry` still needs, in download order. The Neural Engine package is the speech
/// recognition model on Macs that run it, so it comes with a fresh speech model download; once
/// speech recognition is installed without it, it's offered on its own instead of blocking
/// voice, unless an interrupted download of it is waiting to finish.
pub fn missing_components(entry: VoiceEntry, availability: Availability) -> Vec<VoiceComponent> {
    let mut missing = Vec::new();
    let speech_input_missing = availability.speech_input != Presence::Installed;
    if speech_input_missing {
        missing.push(VoiceComponent::SpeechInput);
    }
    let neural_engine_wanted = match availability.neural_engine {
        Presence::Installed => false,
        Presence::Partial => true,
        Presence::Missing => speech_input_missing && !availability.neural_engine_offered,
    };
    if availability.neural_engine_supported && neural_engine_wanted {
        missing.push(VoiceComponent::NeuralEngine);
    }
    let needs_speech_output = match entry {
        VoiceEntry::Conversation => true,
        VoiceEntry::PushToTalk | VoiceEntry::HandsFree => false,
    };
    if needs_speech_output
        && availability
            .speech_output
            .is_some_and(|presence| presence != Presence::Installed)
    {
        missing.push(VoiceComponent::SpeechOutput);
    }
    missing
}

/// One model in a download: its files in order and how much of them was on disk at the start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    pub component: VoiceComponent,
    /// Each model in the component's bundle, with its size.
    pub parts: Vec<(&'static str, u64)>,
    pub already_downloaded: u64,
}

impl PlanItem {
    fn new(component: VoiceComponent, bundle: &ModelBundle) -> Self {
        let already_downloaded = match bundle.status() {
            ModelStatus::NotInstalled => 0,
            ModelStatus::Partial { downloaded } => downloaded,
            ModelStatus::Installed => bundle.total_size(),
        };
        Self {
            component,
            parts: bundle
                .stores()
                .iter()
                .map(|store| {
                    (
                        part_label(component, store.spec()),
                        store.spec().total_size(),
                    )
                })
                .collect(),
            already_downloaded,
        }
    }

    pub fn total(&self) -> u64 {
        self.parts.iter().map(|(_, size)| size).sum()
    }

    pub fn remaining(&self) -> u64 {
        self.total().saturating_sub(self.already_downloaded)
    }

    /// The part being downloaded once `downloaded` bytes of the bundle are in place.
    pub fn current_part(&self, downloaded: u64) -> &'static str {
        let mut end = 0;
        for (label, size) in &self.parts {
            end += size;
            if downloaded < end {
                return label;
            }
        }
        self.parts
            .last()
            .map_or("speech models", |(label, _)| label)
    }
}

fn part_label(component: VoiceComponent, spec: &ModelSpec) -> &'static str {
    match component {
        VoiceComponent::NeuralEngine => "Neural Engine speech recognition",
        VoiceComponent::SpeechOutput => "the voice",
        VoiceComponent::SpeechInput if spec.id == silero_vad_v5().id => "voice detection",
        VoiceComponent::SpeechInput if spec.id == smart_turn_v3_2().id => "end-of-turn detection",
        VoiceComponent::SpeechInput => "speech recognition",
    }
}

/// Bytes still to download for `items`.
pub fn remaining_size(items: &[PlanItem]) -> u64 {
    items.iter().map(PlanItem::remaining).sum()
}

/// Progress across `items`, given each item's downloaded bytes counted against its whole bundle.
/// Only bytes fetched in this download count, so it starts at zero after a resume.
pub fn overall_progress(items: &[PlanItem], downloaded: &[u64]) -> DownloadProgress {
    let total = remaining_size(items);
    let done = items
        .iter()
        .zip(downloaded)
        .map(|(item, downloaded)| {
            downloaded
                .saturating_sub(item.already_downloaded)
                .min(item.remaining())
        })
        .sum();
    DownloadProgress {
        downloaded: done,
        total,
    }
}

/// An item's downloaded bytes against its bundle while it's in `state`.
fn downloaded_bytes(item: &PlanItem, state: &ModelState) -> u64 {
    match state {
        ModelState::Installed => item.total(),
        ModelState::Downloading(progress) => progress.downloaded.max(item.already_downloaded),
        ModelState::NotInstalled | ModelState::AwaitingConsent | ModelState::Failed(_) => {
            item.already_downloaded
        }
    }
}

/// What a download of several models does after the current one's state changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Progress,
    /// Start the item at this index.
    StartNext(usize),
    Done,
    Failed(String),
    Cancelled,
}

pub fn next_step(item_count: usize, current: usize, state: &ModelState) -> Step {
    match state {
        ModelState::Downloading(_) => Step::Progress,
        ModelState::Installed if current + 1 < item_count => Step::StartNext(current + 1),
        ModelState::Installed => Step::Done,
        ModelState::Failed(message) => Step::Failed(message.clone()),
        ModelState::NotInstalled | ModelState::AwaitingConsent => Step::Cancelled,
    }
}

/// Whether a download that finished `elapsed` after the click still starts `entry`.
pub fn should_auto_start(
    entry: VoiceEntry,
    elapsed: Duration,
    requested_window: WindowId,
    active_window: Option<WindowId>,
) -> bool {
    let starts_something = match entry {
        VoiceEntry::HandsFree | VoiceEntry::Conversation => true,
        // Push-to-talk records only while the user holds the control.
        VoiceEntry::PushToTalk => false,
    };
    starts_something && elapsed <= AUTO_START_WINDOW && active_window == Some(requested_window)
}

/// Formats a byte count in gigabytes from 1 GB up, else in megabytes.
pub fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format_gigabytes(bytes)
    } else {
        format_megabytes(bytes)
    }
}

/// What the download is called in its toasts.
fn download_noun(components: &[VoiceComponent]) -> &'static str {
    match components {
        [VoiceComponent::SpeechOutput] => "a voice",
        _ => "speech models",
    }
}

pub fn consent_prompt(components: &[VoiceComponent], size: u64) -> String {
    match components {
        [VoiceComponent::SpeechOutput] => format!(
            "Voice conversation needs to download a voice ({}). Download now?",
            format_size(size)
        ),
        _ => format!(
            "Voice needs to download speech models ({}). Download now?",
            format_size(size)
        ),
    }
}

pub fn progress_message(
    components: &[VoiceComponent],
    progress: DownloadProgress,
    part: &str,
) -> String {
    let noun = download_noun(components);
    match progress.percent() {
        None => format!("Downloading {noun}: starting"),
        Some(percent) => format!(
            "Downloading {noun}: {percent}% ({} of {}), {part}",
            format_size(progress.downloaded),
            format_size(progress.total)
        ),
    }
}

/// The click a download answers.
#[derive(Debug, Clone, Copy)]
struct Request {
    entry: VoiceEntry,
    window_id: WindowId,
    at: Instant,
}

enum SetupState {
    Idle,
    AwaitingConsent(Request),
    Downloading {
        request: Request,
        items: Vec<PlanItem>,
        current: usize,
        last_downloaded: u64,
    },
    Failed(Request),
}

pub enum VoiceSetupEvent {
    /// Every model `entry` needs finished downloading; start it in `window_id`.
    AutoStart {
        entry: VoiceEntry,
        window_id: WindowId,
    },
}

/// Runs the downloads of the voice models one after another under one consent and one toast.
pub struct VoiceSetup {
    state: SetupState,
}

impl VoiceSetup {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.subscribe_to_model(&LocalVoiceModel::handle(ctx), |me, _, event, ctx| {
            let LocalVoiceModelEvent::StateChanged = event;
            me.on_model_changed(ctx);
        });
        ctx.subscribe_to_model(&AcceleratedSpeechModel::handle(ctx), |me, _, event, ctx| {
            let LocalVoiceModelEvent::StateChanged = event;
            me.on_model_changed(ctx);
        });
        ctx.subscribe_to_model(&SpeechOutputModel::handle(ctx), |me, _, event, ctx| {
            let LocalVoiceModelEvent::StateChanged = event;
            me.on_model_changed(ctx);
        });
        Self {
            state: SetupState::Idle,
        }
    }

    /// Whether `entry` can start now. Otherwise asks to download what it needs, or shows the
    /// running download.
    fn ensure_ready(
        &mut self,
        entry: VoiceEntry,
        window_id: WindowId,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let request = Request {
            entry,
            window_id,
            at: Instant::now(),
        };
        let missing = missing_components(entry, availability(ctx));
        if let SetupState::Downloading {
            request: running,
            items,
            ..
        } = &mut self.state
        {
            if missing.is_empty() {
                return true;
            }
            if missing
                .iter()
                .all(|component| items.iter().any(|item| item.component == *component))
            {
                *running = request;
            }
            self.show_progress(window_id, ctx);
            return false;
        }
        if missing.is_empty() {
            offer_accelerated_speech_model(window_id, ctx);
            return true;
        }
        // A download started from settings shows its own progress.
        for component in &missing {
            if matches!(component_state(*component, ctx), ModelState::Downloading(_)) {
                show_model_progress(*component, window_id, ctx);
                return false;
            }
        }
        let items = plan_items(&missing, ctx);
        let toast = DismissibleToast::default(consent_prompt(&missing, remaining_size(&items)))
            .with_link(
                ToastLink::new("Download".to_owned())
                    .with_onclick_action(WorkspaceAction::DownloadVoiceModels),
            )
            .with_object_id(TOAST_ID.to_owned());
        self.move_toast(window_id, ctx);
        add_toast(toast, window_id, ctx);
        self.state = SetupState::AwaitingConsent(request);
        false
    }

    /// Downloads what the pending request needs, after the user agreed in `window_id`.
    pub fn download(&mut self, window_id: WindowId, ctx: &mut ModelContext<Self>) {
        let request = match &self.state {
            SetupState::AwaitingConsent(request) | SetupState::Failed(request) => Request {
                window_id,
                ..*request
            },
            SetupState::Idle | SetupState::Downloading { .. } => return,
        };
        let missing = missing_components(request.entry, availability(ctx));
        if missing.contains(&VoiceComponent::NeuralEngine) {
            AISettings::handle(ctx).update(ctx, |settings, ctx| {
                if let Err(error) = settings
                    .voice_neural_engine_model_offered
                    .set_value(true, ctx)
                {
                    log::warn!("Couldn't record the Neural Engine speech model offer: {error}");
                }
            });
        }
        let items = plan_items(&missing, ctx);
        log::info!(
            "Voice setup: downloading {:?} ({}) for {:?}",
            missing,
            format_size(remaining_size(&items)),
            request.entry
        );
        self.state = SetupState::Downloading {
            request,
            items,
            current: 0,
            last_downloaded: 0,
        };
        self.start_from(0, ctx);
        self.show_progress(window_id, ctx);
    }

    /// Cancels the running download; finished models stay, and partial files resume next time.
    pub fn cancel(&mut self, ctx: &mut ModelContext<Self>) {
        let SetupState::Downloading {
            request,
            items,
            current,
            ..
        } = &self.state
        else {
            return;
        };
        let component = items[*current].component;
        let window_id = request.window_id;
        log::info!("Voice setup: download cancelled");
        self.state = SetupState::Idle;
        remove_toast(window_id, ctx);
        cancel_component(component, ctx);
    }

    /// Starts the first item from `index` that isn't installed, or finishes.
    fn start_from(&mut self, index: usize, ctx: &mut ModelContext<Self>) {
        let SetupState::Downloading { items, .. } = &self.state else {
            return;
        };
        let next = items
            .iter()
            .enumerate()
            .skip(index)
            .find(|(_, item)| !component_state(item.component, ctx).is_installed())
            .map(|(index, item)| (index, item.component));
        let Some((index, component)) = next else {
            self.finish(ctx);
            return;
        };
        if let SetupState::Downloading { current, .. } = &mut self.state {
            *current = index;
        }
        start_component(component, ctx);
        self.refresh_progress(ctx);
    }

    fn on_model_changed(&mut self, ctx: &mut ModelContext<Self>) {
        let SetupState::Downloading { items, current, .. } = &self.state else {
            return;
        };
        let (count, index) = (items.len(), *current);
        let state = component_state(items[index].component, ctx);
        match next_step(count, index, &state) {
            Step::Progress => self.refresh_progress(ctx),
            Step::StartNext(next) => self.start_from(next, ctx),
            Step::Done => self.finish(ctx),
            Step::Failed(message) => self.fail(message, ctx),
            Step::Cancelled => {
                if let SetupState::Downloading { request, .. } = &self.state {
                    remove_toast(request.window_id, ctx);
                }
                self.state = SetupState::Idle;
            }
        }
    }

    fn refresh_progress(&mut self, ctx: &mut ModelContext<Self>) {
        let SetupState::Downloading {
            request,
            items,
            current,
            last_downloaded,
        } = &mut self.state
        else {
            return;
        };
        let downloaded: Vec<u64> = items
            .iter()
            .map(|item| downloaded_bytes(item, &component_state(item.component, ctx)))
            .collect();
        *last_downloaded = downloaded[*current];
        let toast = progress_toast(items, *current, &downloaded);
        let window_id = request.window_id;
        ToastStack::handle(ctx).update(ctx, |stack, ctx| {
            stack.update_toast_in_place(toast, window_id, ctx);
        });
    }

    /// Shows the running download's progress toast in `window_id`.
    fn show_progress(&mut self, window_id: WindowId, ctx: &mut ModelContext<Self>) {
        self.move_toast(window_id, ctx);
        let SetupState::Downloading {
            request,
            items,
            current,
            ..
        } = &mut self.state
        else {
            return;
        };
        request.window_id = window_id;
        let downloaded: Vec<u64> = items
            .iter()
            .map(|item| downloaded_bytes(item, &component_state(item.component, ctx)))
            .collect();
        add_toast(progress_toast(items, *current, &downloaded), window_id, ctx);
    }

    /// Removes this download's toast from its window when it moves to `window_id`.
    fn move_toast(&self, window_id: WindowId, ctx: &mut ModelContext<Self>) {
        let previous = match &self.state {
            SetupState::AwaitingConsent(request)
            | SetupState::Failed(request)
            | SetupState::Downloading { request, .. } => request.window_id,
            SetupState::Idle => return,
        };
        if previous != window_id {
            remove_toast(previous, ctx);
        }
    }

    fn fail(&mut self, message: String, ctx: &mut ModelContext<Self>) {
        let SetupState::Downloading {
            request,
            items,
            current,
            last_downloaded,
        } = &self.state
        else {
            return;
        };
        let part = items[*current].current_part(*last_downloaded);
        log::warn!("Voice setup: downloading {part} failed: {message}");
        let request = *request;
        let toast = DismissibleToast::error(format!("Downloading {part} failed: {message}"))
            .with_link(
                ToastLink::new("Retry".to_owned())
                    .with_onclick_action(WorkspaceAction::DownloadVoiceModels),
            )
            .with_object_id(TOAST_ID.to_owned());
        add_toast(toast, request.window_id, ctx);
        self.state = SetupState::Failed(request);
    }

    fn finish(&mut self, ctx: &mut ModelContext<Self>) {
        let SetupState::Downloading { request, items, .. } = &self.state else {
            return;
        };
        let request = *request;
        let components: Vec<VoiceComponent> = items.iter().map(|item| item.component).collect();
        self.state = SetupState::Idle;
        remove_toast(request.window_id, ctx);
        let message = match components.as_slice() {
            [VoiceComponent::SpeechOutput] => "Voice ready",
            _ => "Speech models ready",
        };
        ToastStack::handle(ctx).update(ctx, |stack, ctx| {
            stack.add_ephemeral_toast(
                DismissibleToast::success(message.to_owned()),
                request.window_id,
                ctx,
            );
        });
        let auto_start = should_auto_start(
            request.entry,
            request.at.elapsed(),
            request.window_id,
            ctx.windows().active_window(),
        );
        log::info!(
            "Voice setup: models ready for {:?}; starting it: {auto_start}",
            request.entry
        );
        if auto_start {
            ctx.emit(VoiceSetupEvent::AutoStart {
                entry: request.entry,
                window_id: request.window_id,
            });
        }
    }
}

impl Entity for VoiceSetup {
    type Event = VoiceSetupEvent;
}

impl SingletonEntity for VoiceSetup {}

/// Checks that the models `entry` needs are installed, offering to download them in one prompt
/// when they aren't. Always `true` when voice isn't transcribed locally.
#[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
pub fn ensure_voice_ready(entry: VoiceEntry, window_id: WindowId, ctx: &mut AppContext) -> bool {
    if !is_local_voice_enabled() {
        return true;
    }
    VoiceSetup::handle(ctx).update(ctx, |setup, ctx| setup.ensure_ready(entry, window_id, ctx))
}

fn availability(ctx: &mut AppContext) -> Availability {
    LocalVoiceModel::handle(ctx).update(ctx, |model, ctx| model.sync_with_disk(ctx));
    AcceleratedSpeechModel::handle(ctx).update(ctx, |model, ctx| model.sync_with_disk(ctx));
    SpeechOutputModel::handle(ctx).update(ctx, |model, ctx| model.sync_with_disk(ctx));
    let engine =
        SpeechOutputEngine::from_setting(&AISettings::as_ref(ctx).conversation_speech_engine);
    let speech_output = SpeechOutputModel::as_ref(ctx);
    Availability {
        speech_input: LocalVoiceModel::as_ref(ctx).bundle().status().into(),
        neural_engine: AcceleratedSpeechModel::as_ref(ctx).bundle().status().into(),
        speech_output: engine
            .spec()
            .is_some()
            .then(|| speech_output.bundle().status().into()),
        neural_engine_supported: supports_neural_engine_package()
            && transcription_acceleration() == local_transcription::tdt::Acceleration::Auto,
        neural_engine_offered: *AISettings::as_ref(ctx).voice_neural_engine_model_offered,
    }
}

fn plan_items(components: &[VoiceComponent], ctx: &AppContext) -> Vec<PlanItem> {
    components
        .iter()
        .map(|component| {
            let bundle = match component {
                VoiceComponent::SpeechInput => LocalVoiceModel::as_ref(ctx).bundle(),
                VoiceComponent::NeuralEngine => AcceleratedSpeechModel::as_ref(ctx).bundle(),
                VoiceComponent::SpeechOutput => SpeechOutputModel::as_ref(ctx).bundle(),
            };
            PlanItem::new(*component, bundle)
        })
        .collect()
}

fn component_state(component: VoiceComponent, ctx: &AppContext) -> ModelState {
    match component {
        VoiceComponent::SpeechInput => LocalVoiceModel::as_ref(ctx).state().clone(),
        VoiceComponent::NeuralEngine => AcceleratedSpeechModel::as_ref(ctx).state().clone(),
        VoiceComponent::SpeechOutput => SpeechOutputModel::as_ref(ctx).state().clone(),
    }
}

fn start_component(component: VoiceComponent, ctx: &mut AppContext) {
    fn start<K: VoiceModelKind>(ctx: &mut AppContext) {
        VoiceModel::<K>::handle(ctx).update(ctx, |model, ctx| model.download_quietly(ctx));
    }
    match component {
        VoiceComponent::SpeechInput => start::<SpeechInputKind>(ctx),
        VoiceComponent::NeuralEngine => start::<AcceleratedSpeechKind>(ctx),
        VoiceComponent::SpeechOutput => start::<SpeechOutputKind>(ctx),
    }
}

fn cancel_component(component: VoiceComponent, ctx: &mut AppContext) {
    fn cancel<K: VoiceModelKind>(ctx: &mut AppContext) {
        VoiceModel::<K>::handle(ctx).update(ctx, |model, ctx| model.cancel_download(ctx));
    }
    match component {
        VoiceComponent::SpeechInput => cancel::<SpeechInputKind>(ctx),
        VoiceComponent::NeuralEngine => cancel::<AcceleratedSpeechKind>(ctx),
        VoiceComponent::SpeechOutput => cancel::<SpeechOutputKind>(ctx),
    }
}

fn show_model_progress(component: VoiceComponent, window_id: WindowId, ctx: &mut AppContext) {
    fn show<K: VoiceModelKind>(window_id: WindowId, ctx: &mut AppContext) {
        VoiceModel::<K>::handle(ctx)
            .update(ctx, |model, ctx| model.show_progress_toast(window_id, ctx));
    }
    match component {
        VoiceComponent::SpeechInput => show::<SpeechInputKind>(window_id, ctx),
        VoiceComponent::NeuralEngine => show::<AcceleratedSpeechKind>(window_id, ctx),
        VoiceComponent::SpeechOutput => show::<SpeechOutputKind>(window_id, ctx),
    }
}

fn progress_toast(
    items: &[PlanItem],
    current: usize,
    downloaded: &[u64],
) -> DismissibleToast<WorkspaceAction> {
    let components: Vec<VoiceComponent> = items.iter().map(|item| item.component).collect();
    let part = items[current].current_part(downloaded[current]);
    DismissibleToast::default(progress_message(
        &components,
        overall_progress(items, downloaded),
        part,
    ))
    .with_link(
        ToastLink::new("Cancel".to_owned())
            .with_onclick_action(WorkspaceAction::CancelVoiceModelsDownload),
    )
    .with_object_id(TOAST_ID.to_owned())
}

fn add_toast(toast: DismissibleToast<WorkspaceAction>, window_id: WindowId, ctx: &mut AppContext) {
    ToastStack::handle(ctx).update(ctx, |stack, ctx| {
        stack.add_persistent_toast(toast, window_id, ctx);
    });
}

fn remove_toast(window_id: WindowId, ctx: &mut AppContext) {
    ToastStack::handle(ctx).update(ctx, |stack, ctx| {
        stack.remove_toast_by_identifier(TOAST_ID.to_owned(), window_id, ctx);
    });
}

/// Starts the voice mode a finished download was for.
#[cfg(feature = "voice_input")]
pub struct VoiceAutoStart;

#[cfg(feature = "voice_input")]
impl VoiceAutoStart {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.subscribe_to_model(&VoiceSetup::handle(ctx), |_, _, event, ctx| {
            let VoiceSetupEvent::AutoStart { entry, window_id } = *event;
            match entry {
                VoiceEntry::Conversation => {
                    if !super::conversation::ConversationSession::as_ref(ctx).is_active() {
                        super::conversation::toggle_conversation(window_id, None, ctx);
                    }
                }
                VoiceEntry::HandsFree => {
                    if !super::hands_free::HandsFreeSession::as_ref(ctx).is_active() {
                        super::hands_free::toggle_hands_free(window_id, ctx);
                    }
                }
                VoiceEntry::PushToTalk => {}
            }
        });
        Self
    }
}

#[cfg(feature = "voice_input")]
impl Entity for VoiceAutoStart {
    type Event = ();
}

#[cfg(feature = "voice_input")]
impl SingletonEntity for VoiceAutoStart {}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
