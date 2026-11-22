//! The "Recordings" settings page: lists session recordings and plays them back.

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Local, TimeZone as _};
use instant::Instant;
use session_recording::RecordingMetadata;

use settings::Setting as _;
use warp_core::features::FeatureFlag;
use warpui::r#async::Timer;
use warpui::clipboard::ClipboardContent;
use warpui::elements::{
    Container, CrossAxisAlignment, Element, Flex, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, ParentElement, Text,
};
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::ui_components::slider::SliderStateHandle;
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use super::SettingsSection;
use super::settings_page::{
    MatchData, PageTitle, PageType, SettingsPageMeta, SettingsPageViewHandle, SettingsWidget,
    render_body_item_label, render_sub_header,
};
use crate::appearance::Appearance;
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions};
use crate::report_if_error;
use crate::server::server_api::ServerApiProvider;
use crate::terminal::session_recording::SessionRecordingSettings;
use crate::terminal::session_recording::asciicast;
use crate::terminal::session_recording::client::{RecordingsClient, sync_local_store_root};
use crate::terminal::session_recording::naming::{format_duration, format_size};
use crate::terminal::session_recording::player::{PlaybackState, Player};
use crate::terminal::session_recording::settings::default_recordings_dir;
use crate::workspace::WorkspaceAction;

const PAGE_TITLE: &str = "Recordings";
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const SEEK_STEP_SECONDS: f64 = 5.;

#[derive(Default)]
struct RowMouseStates {
    play: MouseStateHandle,
    reveal: MouseStateHandle,
    copy: MouseStateHandle,
    rename: MouseStateHandle,
    delete: MouseStateHandle,
}

struct RecordingRow {
    info: RecordingMetadata,
    mouse_states: RowMouseStates,
}

#[derive(Default)]
struct PlayerControls {
    play_pause: MouseStateHandle,
    restart: MouseStateHandle,
    back: MouseStateHandle,
    forward: MouseStateHandle,
    speed: MouseStateHandle,
    close: MouseStateHandle,
    seek: SliderStateHandle,
}

struct PlaybackSession {
    player: Player,
    name: String,
    last_frame: Instant,
    controls: PlayerControls,
}

pub struct RecordingsPageView {
    page: PageType<Self>,
    rows: Vec<RecordingRow>,
    list_error: Option<String>,
    playback: Option<PlaybackSession>,
    /// Index of the row whose name is being edited.
    renaming: Option<usize>,
    /// Index of the row awaiting delete confirmation.
    confirming_delete: Option<usize>,
    rename_editor: ViewHandle<EditorView>,
    directory_editor: ViewHandle<EditorView>,
    is_frame_scheduled: bool,
}

#[derive(Debug, Clone)]
pub enum RecordingsPageAction {
    Refresh,
    Play(usize),
    Reveal(usize),
    Copy(usize),
    StartRename(usize),
    Delete(usize),
    TogglePlayPause,
    Restart,
    SeekBy(f64),
    SeekToFraction(f32),
    CycleSpeed,
    ClosePlayer,
    ResetDirectory,
    ToggleRecordInput,
}

impl RecordingsPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let rename_editor = Self::single_line_editor(ctx);
        ctx.subscribe_to_view(&rename_editor, |me, _, event, ctx| match event {
            EditorEvent::Enter => me.finish_rename(ctx),
            EditorEvent::Escape | EditorEvent::Blurred => {
                me.renaming = None;
                ctx.notify();
            }
            _ => {}
        });
        let directory_editor = Self::single_line_editor(ctx);
        let configured = SessionRecordingSettings::as_ref(ctx)
            .directory
            .value()
            .clone();
        directory_editor.update(ctx, |editor, ctx| editor.set_buffer_text(&configured, ctx));
        ctx.subscribe_to_view(&directory_editor, |me, _, event, ctx| {
            if let EditorEvent::Enter | EditorEvent::Blurred = event {
                me.save_directory(ctx);
            }
        });

        let mut view = Self {
            page: PageType::new_uncategorized(
                vec![
                    Box::new(DirectoryWidget::default()),
                    Box::new(PlayerWidget),
                    Box::new(RecordingsListWidget),
                ],
                Some(PageTitle::new(PAGE_TITLE)),
            ),
            rows: Vec::new(),
            list_error: None,
            playback: None,
            renaming: None,
            confirming_delete: None,
            rename_editor,
            directory_editor,
            is_frame_scheduled: false,
        };
        view.refresh(ctx);
        view
    }

    fn single_line_editor(ctx: &mut ViewContext<Self>) -> ViewHandle<EditorView> {
        ctx.add_typed_action_view(|ctx| {
            let options = SingleLineEditorOptions {
                text: TextOptions {
                    font_size_override: Some(Appearance::as_ref(ctx).ui_font_size()),
                    ..Default::default()
                },
                ..Default::default()
            };
            EditorView::single_line(options, ctx)
        })
    }

    fn client(ctx: &AppContext) -> Arc<dyn RecordingsClient> {
        ServerApiProvider::as_ref(ctx).get()
    }

    fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        sync_local_store_root(SessionRecordingSettings::as_ref(ctx).recordings_dir());
        let client = Self::client(ctx);
        let _ = ctx.spawn(async move { client.list().await }, |me, result, ctx| {
            match result {
                Ok(recordings) => {
                    me.rows = recordings
                        .into_iter()
                        .map(|info| RecordingRow {
                            info,
                            mouse_states: Default::default(),
                        })
                        .collect();
                    me.list_error = None;
                }
                Err(error) => {
                    me.rows.clear();
                    me.list_error = Some(format!("Couldn't list recordings: {error:#}"));
                }
            }
            me.renaming = None;
            me.confirming_delete = None;
            ctx.notify();
        });
    }

    fn row(&self, index: usize) -> Option<&RecordingMetadata> {
        self.rows.get(index).map(|row| &row.info)
    }

    fn show_error(&mut self, message: String, ctx: &mut ViewContext<Self>) {
        self.list_error = Some(message);
        ctx.notify();
    }

    fn play(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let Some(info) = self.row(index).cloned() else {
            return;
        };
        let client = Self::client(ctx);
        let _ = ctx.spawn(
            async move { client.read(&info.id).await },
            move |me, result, ctx| {
                match result.and_then(|bytes| asciicast::parse(Cursor::new(bytes))) {
                    Ok(cast) => {
                        let mut player = Player::new(cast);
                        player.play();
                        me.playback = Some(PlaybackSession {
                            player,
                            name: info.title,
                            last_frame: Instant::now(),
                            controls: Default::default(),
                        });
                        me.schedule_frame(ctx);
                    }
                    Err(error) => {
                        me.list_error = Some(format!("Couldn't open recording: {error:#}"));
                    }
                }
                ctx.notify();
            },
        );
    }

    fn copy(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let Some(info) = self.row(index).cloned() else {
            return;
        };
        let client = Self::client(ctx);
        let _ = ctx.spawn(
            async move { client.read(&info.id).await },
            move |me, result, ctx| match result.and_then(|bytes| Ok(String::from_utf8(bytes)?)) {
                Ok(contents) => ctx.clipboard().write(ClipboardContent {
                    plain_text: contents,
                    paths: info
                        .local_path
                        .map(|path| vec![path.to_string_lossy().into_owned()]),
                    ..Default::default()
                }),
                Err(error) => me.show_error(format!("Couldn't copy: {error:#}"), ctx),
            },
        );
    }

    fn schedule_frame(&mut self, ctx: &mut ViewContext<Self>) {
        if self.is_frame_scheduled {
            return;
        }
        self.is_frame_scheduled = true;
        let _ = ctx.spawn(Timer::after(FRAME_INTERVAL), |me, _, ctx| {
            me.is_frame_scheduled = false;
            me.advance_frame(ctx);
        });
    }

    fn advance_frame(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(playback) = self.playback.as_mut() else {
            return;
        };
        let now = Instant::now();
        let elapsed = now.duration_since(playback.last_frame).as_secs_f64();
        playback.last_frame = now;
        playback.player.advance(elapsed);
        playback.controls.seek.reset_offset();
        let keep_going = playback.player.state() == PlaybackState::Playing;
        if keep_going {
            self.schedule_frame(ctx);
        }
        ctx.notify();
    }

    fn update_player(&mut self, ctx: &mut ViewContext<Self>, update: impl FnOnce(&mut Player)) {
        let Some(playback) = self.playback.as_mut() else {
            return;
        };
        update(&mut playback.player);
        playback.last_frame = Instant::now();
        playback.controls.seek.reset_offset();
        if playback.player.state() == PlaybackState::Playing {
            self.schedule_frame(ctx);
        }
        ctx.notify();
    }

    fn start_rename(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let Some(id) = self.row(index).map(|info| info.id.clone()) else {
            return;
        };
        self.renaming = Some(index);
        self.rename_editor.update(ctx, |editor, ctx| {
            editor.set_buffer_text(&id, ctx);
            editor.select_all(ctx);
        });
        ctx.focus(&self.rename_editor);
        ctx.notify();
    }

    fn finish_rename(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(id) = self
            .renaming
            .take()
            .and_then(|index| self.row(index))
            .map(|info| info.id.clone())
        else {
            return;
        };
        let new_name = self.rename_editor.as_ref(ctx).buffer_text(ctx);
        let client = Self::client(ctx);
        let _ = ctx.spawn(
            async move { client.rename(&id, &new_name).await },
            |me, result, ctx| match result {
                Ok(_) => me.refresh(ctx),
                Err(error) => me.show_error(format!("Couldn't rename: {error:#}"), ctx),
            },
        );
    }

    fn delete(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        if self.confirming_delete != Some(index) {
            self.confirming_delete = Some(index);
            ctx.notify();
            return;
        }
        let Some(id) = self.row(index).map(|info| info.id.clone()) else {
            return;
        };
        let client = Self::client(ctx);
        let _ = ctx.spawn(
            async move { client.delete(&id).await },
            |me, result, ctx| match result {
                Ok(()) => me.refresh(ctx),
                Err(error) => me.show_error(format!("Couldn't delete: {error:#}"), ctx),
            },
        );
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
        self.refresh(ctx);
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
            RecordingsPageAction::Refresh => self.refresh(ctx),
            RecordingsPageAction::Play(index) => self.play(*index, ctx),
            RecordingsPageAction::Reveal(index) => {
                if let Some(path) = self.row(*index).and_then(|info| info.local_path.clone()) {
                    ctx.dispatch_typed_action(&WorkspaceAction::OpenInExplorer { path });
                }
            }
            RecordingsPageAction::Copy(index) => self.copy(*index, ctx),
            RecordingsPageAction::StartRename(index) => self.start_rename(*index, ctx),
            RecordingsPageAction::Delete(index) => self.delete(*index, ctx),
            RecordingsPageAction::TogglePlayPause => {
                self.update_player(ctx, Player::toggle_play_pause)
            }
            RecordingsPageAction::Restart => self.update_player(ctx, Player::restart),
            RecordingsPageAction::SeekBy(delta) => {
                let delta = *delta;
                self.update_player(ctx, |player| player.seek(player.position() + delta))
            }
            RecordingsPageAction::SeekToFraction(fraction) => {
                let fraction = *fraction;
                self.update_player(ctx, |player| player.seek_to_fraction(fraction))
            }
            RecordingsPageAction::CycleSpeed => self.update_player(ctx, Player::cycle_speed),
            RecordingsPageAction::ClosePlayer => {
                self.playback = None;
                ctx.notify();
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

    fn on_page_selected(&mut self, _allow_steal_focus: bool, ctx: &mut ViewContext<Self>) {
        self.refresh(ctx);
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

#[derive(Default)]
struct DirectoryWidget {
    record_input: MouseStateHandle,
    refresh: MouseStateHandle,
    reset: MouseStateHandle,
}

impl SettingsWidget for DirectoryWidget {
    type View = RecordingsPageView;

    fn search_terms(&self) -> &str {
        "recordings session recording directory folder location asciinema cast"
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
            .with_child(render_body_item_label::<RecordingsPageAction>(
                "Save recordings to".to_owned(),
                None,
                None,
                Default::default(),
                Default::default(),
                appearance,
            ))
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
                    .with_child(button(
                        appearance,
                        "Refresh",
                        ButtonVariant::Secondary,
                        &self.refresh,
                        RecordingsPageAction::Refresh,
                    ))
                    .finish(),
            )
            .with_child(
                Container::new(sub_text(
                    appearance,
                    format!(
                        "Leave empty for the default: {}. Start a recording from a pane's menu, \
                         the command palette, or /record.",
                        default_recordings_dir().display()
                    ),
                ))
                .with_margin_top(4.)
                .finish(),
            )
            .with_child(
                Container::new(
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
                            "Input is never recorded at password prompts. Secrets are redacted \
                             from output and input."
                                .to_owned(),
                        ))
                        .finish(),
                )
                .with_margin_top(8.)
                .finish(),
            )
            .finish()
    }
}

struct PlayerWidget;

impl SettingsWidget for PlayerWidget {
    type View = RecordingsPageView;

    fn search_terms(&self) -> &str {
        "play playback player replay"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        let Some(playback) = view.playback.as_ref() else {
            return Flex::column().finish();
        };
        let player = &playback.player;
        let controls = &playback.controls;
        let theme = appearance.theme();

        let mut screen = Flex::column();
        for line in player.screen_lines() {
            screen.add_child(
                Text::new(
                    if line.is_empty() {
                        " ".to_owned()
                    } else {
                        line
                    },
                    appearance.monospace_font_family(),
                    appearance.monospace_font_size(),
                )
                .with_color(theme.main_text_color(theme.background()).into())
                .with_selectable(true)
                .finish(),
            );
        }
        let screen = Container::new(screen.finish())
            .with_uniform_padding(8.)
            .with_background(theme.background())
            .with_border(warpui::elements::Border::all(1.).with_border_fill(theme.outline()))
            .finish();

        let play_label = match player.state() {
            PlaybackState::Playing => "Pause",
            PlaybackState::Paused => "Play",
            PlaybackState::Ended => "Replay",
        };
        let slider = appearance
            .ui_builder()
            .slider(controls.seek.clone())
            .with_range(0.0..1.)
            .with_default_value(player.progress())
            .with_style(UiComponentStyles {
                width: Some(240.),
                ..Default::default()
            })
            .on_change(|ctx, _, value| {
                ctx.dispatch_typed_action(RecordingsPageAction::SeekToFraction(value));
            })
            .build()
            .finish();
        let control_row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.)
            .with_children([
                button(
                    appearance,
                    play_label,
                    ButtonVariant::Accent,
                    &controls.play_pause,
                    RecordingsPageAction::TogglePlayPause,
                ),
                button(
                    appearance,
                    "Restart",
                    ButtonVariant::Secondary,
                    &controls.restart,
                    RecordingsPageAction::Restart,
                ),
                button(
                    appearance,
                    "-5s",
                    ButtonVariant::Secondary,
                    &controls.back,
                    RecordingsPageAction::SeekBy(-SEEK_STEP_SECONDS),
                ),
                button(
                    appearance,
                    "+5s",
                    ButtonVariant::Secondary,
                    &controls.forward,
                    RecordingsPageAction::SeekBy(SEEK_STEP_SECONDS),
                ),
                button(
                    appearance,
                    &format!("{}x", player.speed()),
                    ButtonVariant::Secondary,
                    &controls.speed,
                    RecordingsPageAction::CycleSpeed,
                ),
                slider,
                sub_text(
                    appearance,
                    format!(
                        "{} / {}",
                        format_duration(player.position()),
                        format_duration(player.duration())
                    ),
                ),
                button(
                    appearance,
                    "Close",
                    ButtonVariant::Secondary,
                    &controls.close,
                    RecordingsPageAction::ClosePlayer,
                ),
            ])
            .finish();

        Flex::column()
            .with_spacing(8.)
            .with_child(render_sub_header(
                appearance,
                format!("Playing: {}", playback.name),
                None,
            ))
            .with_child(screen)
            .with_child(control_row)
            .finish()
    }
}

struct RecordingsListWidget;

impl SettingsWidget for RecordingsListWidget {
    type View = RecordingsPageView;

    fn search_terms(&self) -> &str {
        "recordings list play rename delete reveal finder"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        let mut column = Flex::column().with_spacing(8.);
        column.add_child(render_sub_header(appearance, "Saved recordings", None));
        if let Some(error) = &view.list_error {
            column.add_child(sub_text(appearance, error.clone()));
        }
        if view.rows.is_empty() && view.list_error.is_none() {
            column.add_child(sub_text(appearance, "No recordings yet.".to_owned()));
        }
        for (index, row) in view.rows.iter().enumerate() {
            column.add_child(render_row(view, index, row, appearance));
        }
        column.finish()
    }
}

fn render_row(
    view: &RecordingsPageView,
    index: usize,
    row: &RecordingRow,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let info = &row.info;
    let states = &row.mouse_states;
    let date = info
        .started_at
        .and_then(|secs| Local.timestamp_opt(secs, 0).single())
        .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default();
    let details = format!(
        "{date}  ·  {}  ·  {}",
        format_duration(info.duration),
        format_size(info.size_bytes)
    );
    let name: Box<dyn Element> = if view.renaming == Some(index) {
        appearance
            .ui_builder()
            .text_input(view.rename_editor.clone())
            .with_style(UiComponentStyles {
                width: Some(280.),
                ..Default::default()
            })
            .build()
            .finish()
    } else {
        Text::new(
            info.title.clone(),
            appearance.ui_font_family(),
            appearance.ui_font_size(),
        )
        .with_color(
            appearance
                .theme()
                .main_text_color(appearance.theme().background())
                .into(),
        )
        .finish()
    };
    let delete_label = if view.confirming_delete == Some(index) {
        "Confirm delete"
    } else {
        "Delete"
    };
    let reveal_label = if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else {
        "Show in folder"
    };
    let mut buttons = vec![button(
        appearance,
        "Play",
        ButtonVariant::Accent,
        &states.play,
        RecordingsPageAction::Play(index),
    )];
    // Only recordings stored on this machine have a folder to show.
    if info.local_path.is_some() {
        buttons.push(button(
            appearance,
            reveal_label,
            ButtonVariant::Secondary,
            &states.reveal,
            RecordingsPageAction::Reveal(index),
        ));
    }
    buttons.extend([button(
        appearance,
        "Copy",
        ButtonVariant::Secondary,
        &states.copy,
        RecordingsPageAction::Copy(index),
    )]);
    // A recording still in progress keeps receiving events under its ID, so it can't be renamed.
    if !info.in_progress {
        buttons.push(button(
            appearance,
            "Rename",
            ButtonVariant::Secondary,
            &states.rename,
            RecordingsPageAction::StartRename(index),
        ));
    }
    buttons.push(button(
        appearance,
        delete_label,
        ButtonVariant::Warn,
        &states.delete,
        RecordingsPageAction::Delete(index),
    ));
    Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_child(
            Flex::column()
                .with_child(name)
                .with_child(sub_text(appearance, details))
                .finish(),
        )
        .with_child(Flex::row().with_spacing(6.).with_children(buttons).finish())
        .finish()
}
