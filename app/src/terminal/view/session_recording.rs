//! Asciicast recording of a terminal pane.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_broadcast::InactiveReceiver;
use chrono::Local;
use settings::Setting as _;
use warpui::elements::{CrossAxisAlignment, Flex, Text};
use warpui::elements::{Hoverable, MouseStateHandle, ParentElement};
use warpui::fonts::{Properties, Style, Weight};
use warpui::platform::Cursor;
use warpui::prelude::Container;
use warpui::{AppContext, Element, SingletonEntity, ViewContext};

use super::TerminalAction;
use crate::appearance::Appearance;
use crate::features::FeatureFlag;
use crate::menu::{MenuItem, MenuItemFields};
use crate::pane_group::pane::view::PaneHeaderAction;
use crate::server::server_api::ServerApiProvider;
use crate::terminal::TerminalView;
use crate::terminal::session_recording::SessionRecordingSettings;
use crate::terminal::session_recording::asciicast::Header;
use crate::terminal::session_recording::client::{RecordingsClient, sync_local_store_root};
use crate::terminal::session_recording::recorder::{
    ActiveRecording, RecordingOptions, UploadPolicy,
};
use crate::view_components::{DismissibleToast, ToastLink};
use crate::workspace::{ToastStack, WorkspaceAction};
use ::session_recording::{CreateRecording, RecordingMetadata};

/// Per-pane recording state.
pub(crate) struct CastRecordingState {
    /// `None` for panes without a local PTY, which can't be recorded.
    pty_reads_rx: Option<InactiveReceiver<Arc<Vec<u8>>>>,
    active: Option<ActiveRecording>,
    is_recording_input: bool,
    indicator_mouse_state: MouseStateHandle,
}

impl CastRecordingState {
    pub(crate) fn new(pty_reads_rx: Option<InactiveReceiver<Arc<Vec<u8>>>>) -> Self {
        Self {
            pty_reads_rx,
            active: None,
            is_recording_input: false,
            indicator_mouse_state: Default::default(),
        }
    }

    pub(crate) fn is_recording(&self) -> bool {
        self.active.is_some()
    }

    pub(crate) fn can_record(&self) -> bool {
        FeatureFlag::SessionRecording.is_enabled()
            && cfg!(feature = "local_fs")
            && self.pty_reads_rx.is_some()
    }

    pub(crate) fn is_recording_input(&self) -> bool {
        self.active.is_some() && self.is_recording_input
    }

    pub(crate) fn input(&self, bytes: &[u8]) {
        if let Some(active) = &self.active {
            active.input(bytes);
        }
    }

    pub(crate) fn command_started(&self, command: &str) {
        if let Some(active) = &self.active
            && !command.trim().is_empty()
        {
            active.marker(command);
        }
    }

    pub(crate) fn on_password_prompt(&self) {
        if let Some(active) = &self.active {
            active.suppress_input_until_next_line();
        }
    }

    pub(crate) fn resize(&self, cols: usize, rows: usize) {
        if let Some(active) = &self.active {
            active.resize(clamp_u16(cols), clamp_u16(rows));
        }
    }
}

fn clamp_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

impl TerminalView {
    pub(crate) fn toggle_cast_recording(&mut self, ctx: &mut ViewContext<Self>) {
        if self.cast_recording.is_recording() {
            self.stop_cast_recording(ctx);
        } else {
            self.start_cast_recording(ctx);
        }
        ctx.notify();
    }

    pub(crate) fn cast_recording_menu_item(&self) -> Option<MenuItem<TerminalAction>> {
        if !self.cast_recording.can_record() {
            return None;
        }
        let label = if self.cast_recording.is_recording() {
            "Stop recording"
        } else {
            "Start recording"
        };
        Some(
            MenuItemFields::new(label)
                .with_on_select_action(TerminalAction::ToggleCastRecording)
                .into_item(),
        )
    }

    fn start_cast_recording(&mut self, ctx: &mut ViewContext<Self>) {
        if !self.cast_recording.can_record() {
            self.show_error_toast("This pane can't be recorded.".to_owned(), ctx);
            return;
        }
        let Some(pty_reads_rx) = self.cast_recording.pty_reads_rx.as_ref() else {
            return;
        };
        let title = self
            .pwd()
            .filter(|pwd| !pwd.is_empty())
            .map(|pwd| format!("{}: {pwd}", self.terminal_title_text()))
            .unwrap_or_else(|| self.terminal_title_text());
        let now = Local::now();
        let settings = SessionRecordingSettings::as_ref(ctx);
        sync_local_store_root(settings.recordings_dir());
        let mut extra_redact_patterns =
            crate::auth::organization::recording_redact_patterns().to_vec();
        extra_redact_patterns.extend(settings.user_redact_patterns());
        let options = RecordingOptions {
            record_input: *settings.record_input.value(),
            extra_redact_patterns,
        };
        let request = CreateRecording {
            header: Header {
                timestamp: Some(now.timestamp()),
                title: Some(title),
                idle_time_limit: Some(settings.idle_time_limit()),
                env: recording_env(),
                ..Header::new(
                    clamp_u16(self.size_info.columns()),
                    clamp_u16(self.size_info.rows()),
                )
            },
            name: self.terminal_title_text(),
        };
        let client: Arc<dyn RecordingsClient> = ServerApiProvider::as_ref(ctx).get();
        let records_input = options.record_input;
        match ActiveRecording::start(
            client,
            request,
            pty_reads_rx.activate_cloned(),
            options,
            UploadPolicy::default(),
        ) {
            Ok(active) => {
                log::info!("Started session recording");
                self.cast_recording.active = Some(active);
                self.cast_recording.is_recording_input = records_input;
                self.show_recording_toast("Recording started".to_owned(), None, ctx);
            }
            Err(error) => {
                log::warn!("Couldn't start session recording: {error}");
                self.show_error_toast(format!("Couldn't start recording: {error}"), ctx);
            }
        }
    }

    fn stop_cast_recording(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(active) = self.cast_recording.active.take() else {
            return;
        };
        let _ = ctx.spawn(active.stop(), |me, result, ctx| match result {
            Ok(RecordingMetadata { id, local_path, .. }) => {
                me.show_recording_toast(format!("Recording saved: {id}.cast"), local_path, ctx);
            }
            Err(error) => {
                me.show_error_toast(format!("Couldn't save recording: {error:#}"), ctx);
            }
        });
    }

    /// `path` adds a "Show in folder" link, for recordings stored on this machine.
    fn show_recording_toast(
        &self,
        message: String,
        path: Option<std::path::PathBuf>,
        ctx: &mut ViewContext<Self>,
    ) {
        let window_id = ctx.window_id();
        ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
            let mut toast = DismissibleToast::default(message);
            if let Some(path) = path {
                toast = toast.with_link(
                    ToastLink::new("Show in folder".to_owned())
                        .with_onclick_action(WorkspaceAction::OpenInExplorer { path }),
                );
            }
            toast_stack.add_ephemeral_toast(toast, window_id, ctx);
        });
    }

    /// The red "REC" pane header indicator; clicking it stops the recording.
    pub(crate) fn render_recording_indicator(&self, app: &AppContext) -> Option<Box<dyn Element>> {
        if !self.cast_recording.is_recording() {
            return None;
        }
        let appearance = Appearance::as_ref(app);
        let red = appearance.theme().terminal_colors().normal.red;
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();
        let indicator = Hoverable::new(
            self.cast_recording.indicator_mouse_state.clone(),
            move |_state| {
                let label = |text: &str| {
                    Text::new(text.to_owned(), font_family, font_size)
                        .with_style(Properties {
                            style: Style::Normal,
                            weight: Weight::Bold,
                        })
                        .with_color(red.into())
                        .with_selectable(false)
                        .finish()
                };
                Container::new(
                    Flex::row()
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_spacing(4.)
                        .with_child(label("\u{25cf}"))
                        .with_child(label("REC"))
                        .finish(),
                )
                .with_horizontal_padding(6.)
                .finish()
            },
        )
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action::<PaneHeaderAction<TerminalAction, TerminalAction>>(
                PaneHeaderAction::CustomAction(TerminalAction::ToggleCastRecording),
            );
        })
        .finish();
        Some(indicator)
    }
}

fn recording_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([("TERM".to_owned(), "xterm-256color".to_owned())]);
    if let Ok(shell) = std::env::var("SHELL") {
        env.insert("SHELL".to_owned(), shell);
    }
    env
}
