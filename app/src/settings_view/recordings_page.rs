//! The "Recordings" settings page: where recordings are saved and what they capture. Browsing and
//! playing recordings happens in the Recordings pane.

use settings::Setting as _;
use warp_core::features::FeatureFlag;
use warpui::elements::{
    Container, CrossAxisAlignment, Element, Flex, MouseStateHandle, ParentElement, Text,
};
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use super::SettingsSection;
use super::settings_page::{
    MatchData, PageTitle, PageType, SettingsPageMeta, SettingsPageViewHandle, SettingsWidget,
    render_body_item_label,
};
use crate::appearance::Appearance;
use crate::editor::{
    EditorOptions, EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions,
};
use crate::report_if_error;
use crate::terminal::session_recording::SessionRecordingSettings;
use crate::terminal::session_recording::client::sync_local_store_root;
use crate::terminal::session_recording::settings::default_recordings_dir;
use crate::workspace::WorkspaceAction;

const PAGE_TITLE: &str = "Recordings";

pub struct RecordingsPageView {
    page: PageType<Self>,
    directory_editor: ViewHandle<EditorView>,
    idle_limit_editor: ViewHandle<EditorView>,
    redact_patterns_editor: ViewHandle<EditorView>,
}

#[derive(Debug, Clone)]
pub enum RecordingsPageAction {
    OpenRecordings,
    ResetDirectory,
    ToggleRecordInput,
}

impl RecordingsPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let settings = SessionRecordingSettings::as_ref(ctx);
        let directory = settings.directory.value().clone();
        let idle_limit = settings.idle_time_limit().to_string();
        let redact_patterns = settings.redact_patterns.value().clone();

        let directory_editor = Self::single_line_editor(&directory, ctx);
        ctx.subscribe_to_view(&directory_editor, |me, _, event, ctx| {
            if let EditorEvent::Enter | EditorEvent::Blurred = event {
                me.save_directory(ctx);
            }
        });
        let idle_limit_editor = Self::single_line_editor(&idle_limit, ctx);
        ctx.subscribe_to_view(&idle_limit_editor, |me, _, event, ctx| {
            if let EditorEvent::Enter | EditorEvent::Blurred = event {
                me.save_idle_limit(ctx);
            }
        });
        let redact_patterns_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = EditorOptions {
                autogrow: true,
                soft_wrap: true,
                text: TextOptions {
                    font_size_override: Some(appearance.ui_font_size()),
                    font_family_override: Some(appearance.monospace_font_family()),
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut editor = EditorView::new(options, ctx);
            editor.set_placeholder_text("One regular expression per line", ctx);
            editor.set_buffer_text(&redact_patterns, ctx);
            editor
        });
        ctx.subscribe_to_view(&redact_patterns_editor, |me, _, event, ctx| {
            if let EditorEvent::Blurred = event {
                me.save_redact_patterns(ctx);
            }
        });

        Self {
            page: PageType::new_uncategorized(
                vec![
                    Box::new(OpenRecordingsWidget::default()),
                    Box::new(DirectoryWidget::default()),
                    Box::new(CaptureWidget::default()),
                ],
                Some(PageTitle::new(PAGE_TITLE)),
            ),
            directory_editor,
            idle_limit_editor,
            redact_patterns_editor,
        }
    }

    fn single_line_editor(text: &str, ctx: &mut ViewContext<Self>) -> ViewHandle<EditorView> {
        let editor = ctx.add_typed_action_view(|ctx| {
            let options = SingleLineEditorOptions {
                text: TextOptions {
                    font_size_override: Some(Appearance::as_ref(ctx).ui_font_size()),
                    ..Default::default()
                },
                ..Default::default()
            };
            EditorView::single_line(options, ctx)
        });
        editor.update(ctx, |editor, ctx| editor.set_buffer_text(text, ctx));
        editor
    }

    fn save_directory(&mut self, ctx: &mut ViewContext<Self>) {
        let value = self
            .directory_editor
            .as_ref(ctx)
            .buffer_text(ctx)
            .trim()
            .to_owned();
        if *SessionRecordingSettings::as_ref(ctx).directory.value() == value {
            return;
        }
        SessionRecordingSettings::handle(ctx).update(ctx, |settings, ctx| {
            report_if_error!(settings.directory.set_value(value, ctx));
        });
        sync_local_store_root(SessionRecordingSettings::as_ref(ctx).recordings_dir());
    }

    fn save_idle_limit(&mut self, ctx: &mut ViewContext<Self>) {
        let text = self.idle_limit_editor.as_ref(ctx).buffer_text(ctx);
        match text.trim().parse::<f32>() {
            Ok(limit) if limit.is_finite() && limit > 0. => {
                SessionRecordingSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.idle_time_limit.set_value(limit, ctx));
                });
            }
            Ok(_) | Err(_) => {
                let current = SessionRecordingSettings::as_ref(ctx)
                    .idle_time_limit()
                    .to_string();
                self.idle_limit_editor
                    .update(ctx, |editor, ctx| editor.set_buffer_text(&current, ctx));
            }
        }
    }

    fn save_redact_patterns(&mut self, ctx: &mut ViewContext<Self>) {
        let value = self.redact_patterns_editor.as_ref(ctx).buffer_text(ctx);
        if *SessionRecordingSettings::as_ref(ctx)
            .redact_patterns
            .value()
            == value
        {
            return;
        }
        SessionRecordingSettings::handle(ctx).update(ctx, |settings, ctx| {
            report_if_error!(settings.redact_patterns.set_value(value, ctx));
        });
    }
}

impl Entity for RecordingsPageView {
    type Event = ();
}

impl View for RecordingsPageView {
    fn ui_name() -> &'static str {
        "RecordingsPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl TypedActionView for RecordingsPageView {
    type Action = RecordingsPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            RecordingsPageAction::OpenRecordings => {
                ctx.dispatch_typed_action(&WorkspaceAction::OpenRecordingsPane)
            }
            RecordingsPageAction::ToggleRecordInput => {
                SessionRecordingSettings::handle(ctx).update(ctx, |settings, ctx| {
                    let enabled = !*settings.record_input.value();
                    report_if_error!(settings.record_input.set_value(enabled, ctx));
                });
                ctx.notify();
            }
            RecordingsPageAction::ResetDirectory => {
                self.directory_editor
                    .update(ctx, |editor, ctx| editor.set_buffer_text("", ctx));
                self.save_directory(ctx);
            }
        }
    }
}

impl SettingsPageMeta for RecordingsPageView {
    fn section() -> SettingsSection {
        SettingsSection::Recordings
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        FeatureFlag::SessionRecording.is_enabled() && cfg!(feature = "local_fs")
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<RecordingsPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<RecordingsPageView>) -> Self {
        SettingsPageViewHandle::Recordings(view_handle)
    }
}

fn button(
    appearance: &Appearance,
    label: &str,
    variant: ButtonVariant,
    mouse_state: &MouseStateHandle,
    action: RecordingsPageAction,
) -> Box<dyn Element> {
    appearance
        .ui_builder()
        .button(variant, mouse_state.clone())
        .with_text_label(label.to_owned())
        .with_style(UiComponentStyles {
            font_size: Some(12.),
            ..Default::default()
        })
        .build()
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
}

fn sub_text(appearance: &Appearance, text: String) -> Box<dyn Element> {
    let theme = appearance.theme();
    Text::new(text, appearance.ui_font_family(), appearance.ui_font_size())
        .with_color(theme.sub_text_color(theme.background()).into())
        .finish()
}

fn label(appearance: &Appearance, text: &str) -> Box<dyn Element> {
    render_body_item_label::<RecordingsPageAction>(
        text.to_owned(),
        None,
        None,
        Default::default(),
        Default::default(),
        appearance,
    )
}

#[derive(Default)]
struct OpenRecordingsWidget {
    open: MouseStateHandle,
}

impl SettingsWidget for OpenRecordingsWidget {
    type View = RecordingsPageView;

    fn search_terms(&self) -> &str {
        "recordings open play playback list asciinema cast"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.)
            .with_child(button(
                appearance,
                "Open Recordings",
                ButtonVariant::Accent,
                &self.open,
                RecordingsPageAction::OpenRecordings,
            ))
            .with_child(sub_text(
                appearance,
                "Browse, play, rename, and delete recordings. Start a recording from a pane's \
                 menu, the command palette, or /record."
                    .to_owned(),
            ))
            .finish()
    }
}

#[derive(Default)]
struct DirectoryWidget {
    reset: MouseStateHandle,
}

impl SettingsWidget for DirectoryWidget {
    type View = RecordingsPageView;

    fn search_terms(&self) -> &str {
        "recordings session recording directory folder location"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        let input = appearance
            .ui_builder()
            .text_input(view.directory_editor.clone())
            .with_style(UiComponentStyles {
                width: Some(360.),
                ..Default::default()
            })
            .build()
            .finish();
        Flex::column()
            .with_child(label(appearance, "Save recordings to"))
            .with_child(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(8.)
                    .with_child(input)
                    .with_child(button(
                        appearance,
                        "Use default",
                        ButtonVariant::Secondary,
                        &self.reset,
                        RecordingsPageAction::ResetDirectory,
                    ))
                    .finish(),
            )
            .with_child(
                Container::new(sub_text(
                    appearance,
                    format!(
                        "Leave empty for the default: {}.",
                        default_recordings_dir().display()
                    ),
                ))
                .with_margin_top(4.)
                .finish(),
            )
            .finish()
    }
}

#[derive(Default)]
struct CaptureWidget {
    record_input: MouseStateHandle,
}

impl SettingsWidget for CaptureWidget {
    type View = RecordingsPageView;

    fn search_terms(&self) -> &str {
        "recordings keyboard input keystrokes redaction redact secrets patterns idle time limit"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let record_input_label = if *SessionRecordingSettings::as_ref(app).record_input.value() {
            "Keyboard input: recorded"
        } else {
            "Keyboard input: not recorded"
        };
        let idle_input = appearance
            .ui_builder()
            .text_input(view.idle_limit_editor.clone())
            .with_style(UiComponentStyles {
                width: Some(80.),
                ..Default::default()
            })
            .build()
            .finish();
        let patterns_input = appearance
            .ui_builder()
            .text_input(view.redact_patterns_editor.clone())
            .with_style(UiComponentStyles {
                width: Some(360.),
                ..Default::default()
            })
            .build()
            .finish();
        Flex::column()
            .with_spacing(8.)
            .with_child(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(8.)
                    .with_child(button(
                        appearance,
                        record_input_label,
                        ButtonVariant::Secondary,
                        &self.record_input,
                        RecordingsPageAction::ToggleRecordInput,
                    ))
                    .with_child(sub_text(
                        appearance,
                        "Input is never recorded at password prompts.".to_owned(),
                    ))
                    .finish(),
            )
            .with_child(label(appearance, "Idle time limit (seconds)"))
            .with_child(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(8.)
                    .with_child(idle_input)
                    .with_child(sub_text(
                        appearance,
                        "With Skip idle on, longer pauses play back at this length.".to_owned(),
                    ))
                    .finish(),
            )
            .with_child(label(appearance, "Extra redaction patterns"))
            .with_child(patterns_input)
            .with_child(sub_text(
                appearance,
                "Common secrets are always redacted from output, input, and command markers."
                    .to_owned(),
            ))
            .finish()
    }
}
