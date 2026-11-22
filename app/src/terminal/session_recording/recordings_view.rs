//! The Recordings pane: lists session recordings and plays them back with their colors.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use instant::Instant;
use pathfinder_color::ColorU;
use warpui::r#async::Timer;
use warpui::clipboard::ClipboardContent;
use warpui::elements::{
    ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container, CrossAxisAlignment,
    Element, Expanded, Fill, Flex, Highlight, HighlightedRange, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, ParentElement, ScrollbarWidth, Shrinkable, Text,
};
use warpui::fonts::{Properties, Style, Weight};
use warpui::keymap::FixedBinding;
use warpui::text_layout::{ClipConfig, TextStyle};
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::ui_components::slider::SliderStateHandle;
use warpui::{
    AppContext, Entity, ModelHandle, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle, WindowId,
};

use super::SessionRecordingSettings;
use super::asciicast;
use super::client::{RecordingsClient, sync_local_store_root};
use super::list::{RecordingRow, RecordingsList};
use super::naming::format_duration;
use super::player::{Chapter, PlaybackState, Player};
use super::screen_style::{Palette, styled_lines};
use crate::appearance::Appearance;
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions};
use crate::pane_group::focus_state::PaneFocusHandle;
use crate::pane_group::pane::view::{self, HeaderContent, StandardHeader, StandardHeaderOptions};
use crate::pane_group::{BackingView, PaneConfiguration, PaneEvent};
use crate::server::server_api::ServerApiProvider;
use crate::settings_view::SettingsSection;
use crate::workspace::{PaneViewLocator, WorkspaceAction};

pub const HEADER_TEXT: &str = "Recordings";
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const SEEK_STEP_SECONDS: f64 = 5.;
const LIST_HEIGHT_WITH_PLAYER: f32 = 200.;
const CHAPTERS_HEIGHT: f32 = 140.;

pub fn init(app: &mut AppContext) {
    use warpui::keymap::macros::*;

    app.add_singleton_model(|_| RecordingsPaneManager::default());
    let context = id!(RecordingsView::ui_name()) & !id!("IMEOpen");
    app.register_fixed_bindings([
        FixedBinding::new(
            "space",
            RecordingsViewAction::TogglePlayPause,
            context.clone(),
        ),
        FixedBinding::new("n", RecordingsViewAction::NextChapter, context.clone()),
        FixedBinding::new("p", RecordingsViewAction::PreviousChapter, context.clone()),
        FixedBinding::new(
            "left",
            RecordingsViewAction::SeekBy(-SEEK_STEP_SECONDS),
            context.clone(),
        ),
        FixedBinding::new(
            "right",
            RecordingsViewAction::SeekBy(SEEK_STEP_SECONDS),
            context,
        ),
    ]);
}

/// The open Recordings pane in each window, so reopening focuses it instead of adding another.
#[derive(Default)]
pub struct RecordingsPaneManager {
    panes: HashMap<WindowId, PaneViewLocator>,
}

impl RecordingsPaneManager {
    pub fn find_pane(&self, window_id: WindowId) -> Option<PaneViewLocator> {
        self.panes.get(&window_id).copied()
    }

    pub fn register_pane(&mut self, window_id: WindowId, locator: PaneViewLocator) {
        self.panes.insert(window_id, locator);
    }

    pub fn deregister_pane(&mut self, window_id: &WindowId) {
        self.panes.remove(window_id);
    }
}

impl Entity for RecordingsPaneManager {
    type Event = ();
}

impl SingletonEntity for RecordingsPaneManager {}

#[derive(Default)]
struct PlayerControls {
    play_pause: MouseStateHandle,
    restart: MouseStateHandle,
    back: MouseStateHandle,
    forward: MouseStateHandle,
    previous: MouseStateHandle,
    next: MouseStateHandle,
    speed: MouseStateHandle,
    skip_idle: MouseStateHandle,
    close: MouseStateHandle,
    seek: SliderStateHandle,
    chapters_scroll: ClippedScrollStateHandle,
}

struct PlaybackSession {
    player: Player,
    name: String,
    chapters: Vec<Chapter>,
    chapter_mouse_states: Vec<MouseStateHandle>,
    last_frame: Instant,
    controls: PlayerControls,
}

#[derive(Default)]
struct ToolbarMouseStates {
    refresh: MouseStateHandle,
    settings: MouseStateHandle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingsViewEvent {
    Pane(PaneEvent),
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecordingsViewAction {
    Refresh,
    OpenSettings,
    Play(usize),
    Reveal(usize),
    Copy(usize),
    StartRename(usize),
    Delete(usize),
    TogglePlayPause,
    Restart,
    SeekBy(f64),
    SeekToFraction(f32),
    SeekTo(f64),
    NextChapter,
    PreviousChapter,
    CycleSpeed,
    ToggleSkipIdle,
    ClosePlayer,
}

pub struct RecordingsView {
    list: RecordingsList,
    playback: Option<PlaybackSession>,
    rename_editor: ViewHandle<EditorView>,
    is_frame_scheduled: bool,
    toolbar: ToolbarMouseStates,
    list_scroll: ClippedScrollStateHandle,
    pane_configuration: ModelHandle<PaneConfiguration>,
    focus_handle: Option<PaneFocusHandle>,
}

impl RecordingsView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let pane_configuration = ctx.add_model(|_| PaneConfiguration::new(HEADER_TEXT));
        let rename_editor = ctx.add_typed_action_view(|ctx| {
            let options = SingleLineEditorOptions {
                text: TextOptions {
                    font_size_override: Some(Appearance::as_ref(ctx).ui_font_size()),
                    ..Default::default()
                },
                ..Default::default()
            };
            EditorView::single_line(options, ctx)
        });
        ctx.subscribe_to_view(&rename_editor, |me, _, event, ctx| match event {
            EditorEvent::Enter => me.finish_rename(ctx),
            EditorEvent::Escape | EditorEvent::Blurred => {
                me.list.cancel_rename();
                ctx.notify();
            }
            _ => {}
        });
        let mut view = Self {
            list: RecordingsList::default(),
            playback: None,
            rename_editor,
            is_frame_scheduled: false,
            toolbar: Default::default(),
            list_scroll: Default::default(),
            pane_configuration,
            focus_handle: None,
        };
        view.refresh(ctx);
        view
    }

    pub fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    pub fn focus(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.focus_self();
    }

    fn client(ctx: &AppContext) -> Arc<dyn RecordingsClient> {
        ServerApiProvider::as_ref(ctx).get()
    }

    pub fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        sync_local_store_root(SessionRecordingSettings::as_ref(ctx).recordings_dir());
        let client = Self::client(ctx);
        let _ = ctx.spawn(async move { client.list().await }, |me, result, ctx| {
            me.list.set_listing(result);
            ctx.notify();
        });
    }

    fn show_error(&mut self, message: String, ctx: &mut ViewContext<Self>) {
        self.list.set_error(message);
        ctx.notify();
    }

    fn play(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let Some(info) = self.list.row(index).cloned() else {
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
                        let chapters = player.chapters();
                        me.playback = Some(PlaybackSession {
                            chapter_mouse_states: chapters
                                .iter()
                                .map(|_| Default::default())
                                .collect(),
                            chapters,
                            player,
                            name: info.title,
                            last_frame: Instant::now(),
                            controls: Default::default(),
                        });
                        me.schedule_frame(ctx);
                        ctx.focus_self();
                    }
                    Err(error) => me
                        .list
                        .set_error(format!("Couldn't open recording: {error:#}")),
                }
                ctx.notify();
            },
        );
    }

    fn copy(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let Some(info) = self.list.row(index).cloned() else {
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
        if playback.player.state() == PlaybackState::Playing {
            self.schedule_frame(ctx);
        }
        ctx.notify();
    }

    fn update_player(&mut self, ctx: &mut ViewContext<Self>, update: impl FnOnce(&mut Player)) {
        let Some(playback) = self.playback.as_mut() else {
            return;
        };
        update(&mut playback.player);
        // Chapter times depend on whether idle time is skipped.
        playback.chapters = playback.player.chapters();
        playback.last_frame = Instant::now();
        playback.controls.seek.reset_offset();
        if playback.player.state() == PlaybackState::Playing {
            self.schedule_frame(ctx);
        }
        ctx.notify();
    }

    fn start_rename(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let Some(id) = self.list.start_rename(index) else {
            return;
        };
        self.rename_editor.update(ctx, |editor, ctx| {
            editor.set_buffer_text(&id, ctx);
            editor.select_all(ctx);
        });
        ctx.focus(&self.rename_editor);
        ctx.notify();
    }

    fn finish_rename(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(id) = self.list.finish_rename() else {
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
        let Some(id) = self.list.request_delete(index) else {
            ctx.notify();
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

    fn render_toolbar(&self, appearance: &Appearance) -> Box<dyn Element> {
        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(sub_text(
                appearance,
                "Start a recording from a pane's menu, the command palette, or /record.".to_owned(),
            ))
            .with_child(
                Flex::row()
                    .with_spacing(6.)
                    .with_child(button(
                        appearance,
                        "Refresh",
                        ButtonVariant::Secondary,
                        &self.toolbar.refresh,
                        RecordingsViewAction::Refresh,
                    ))
                    .with_child(button(
                        appearance,
                        "Recording settings",
                        ButtonVariant::Secondary,
                        &self.toolbar.settings,
                        RecordingsViewAction::OpenSettings,
                    ))
                    .finish(),
            )
            .finish()
    }

    fn render_list(&self, appearance: &Appearance) -> Box<dyn Element> {
        let mut column = Flex::column().with_spacing(8.);
        if let Some(error) = self.list.error() {
            column.add_child(sub_text(appearance, error.to_owned()));
        }
        if self.list.is_empty() && self.list.error().is_none() {
            column.add_child(sub_text(appearance, "No recordings yet.".to_owned()));
        }
        for (index, row) in self.list.rows().iter().enumerate() {
            column.add_child(self.render_row(index, row, appearance));
        }
        let theme = appearance.theme();
        ClippedScrollable::vertical(
            self.list_scroll.clone(),
            column.finish(),
            ScrollbarWidth::Auto,
            theme.disabled_text_color(theme.background()).into(),
            theme.main_text_color(theme.background()).into(),
            Fill::None,
        )
        .finish()
    }

    fn render_row(
        &self,
        index: usize,
        row: &RecordingRow,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let info = &row.info;
        let states = &row.mouse_states;
        let theme = appearance.theme();
        let name: Box<dyn Element> = if self.list.renaming() == Some(index) {
            appearance
                .ui_builder()
                .text_input(self.rename_editor.clone())
                .with_style(UiComponentStyles {
                    width: Some(280.),
                    ..Default::default()
                })
                .build()
                .finish()
        } else {
            let mut title = Flex::row().with_spacing(6.).with_child(
                Text::new(
                    info.title.clone(),
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_color(theme.main_text_color(theme.background()).into())
                .finish(),
            );
            if info.in_progress {
                title.add_child(
                    Container::new(
                        Text::new(
                            "Recording",
                            appearance.ui_font_family(),
                            appearance.ui_font_size() - 2.,
                        )
                        .with_color(theme.background().into_solid())
                        .finish(),
                    )
                    .with_horizontal_padding(6.)
                    .with_vertical_padding(1.)
                    .with_background(theme.ui_error_color())
                    .with_corner_radius(warpui::elements::CornerRadius::with_all(
                        warpui::elements::Radius::Pixels(4.),
                    ))
                    .finish(),
                );
            }
            title.finish()
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
            RecordingsViewAction::Play(index),
        )];
        // Only recordings stored on this machine have a folder to show.
        if info.local_path.is_some() {
            buttons.push(button(
                appearance,
                reveal_label,
                ButtonVariant::Secondary,
                &states.reveal,
                RecordingsViewAction::Reveal(index),
            ));
        }
        buttons.push(button(
            appearance,
            "Copy",
            ButtonVariant::Secondary,
            &states.copy,
            RecordingsViewAction::Copy(index),
        ));
        if row.can_rename() {
            buttons.push(button(
                appearance,
                "Rename",
                ButtonVariant::Secondary,
                &states.rename,
                RecordingsViewAction::StartRename(index),
            ));
        }
        buttons.push(button(
            appearance,
            if self.list.is_confirming_delete(index) {
                "Confirm delete"
            } else {
                "Delete"
            },
            ButtonVariant::Warn,
            &states.delete,
            RecordingsViewAction::Delete(index),
        ));
        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Shrinkable::new(
                    1.,
                    Flex::column()
                        .with_child(name)
                        .with_child(sub_text(appearance, row.details()))
                        .finish(),
                )
                .finish(),
            )
            .with_child(Flex::row().with_spacing(6.).with_children(buttons).finish())
            .finish()
    }

    fn render_player(
        &self,
        playback: &PlaybackSession,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let player = &playback.player;
        let controls = &playback.controls;
        let theme = appearance.theme();
        let palette = palette(appearance);

        let mut screen = Flex::column();
        for runs in styled_lines(player.screen(), &palette) {
            let mut text = String::new();
            let mut highlights = Vec::new();
            for run in &runs {
                let start = text.chars().count();
                text.push_str(&run.text);
                let end = text.chars().count();
                let highlight = Highlight::new()
                    .with_text_style(TextStyle {
                        foreground_color: Some(run.foreground),
                        background_color: run.background,
                        underline_color: run.underline.then_some(run.foreground),
                        ..Default::default()
                    })
                    .with_properties(Properties {
                        weight: if run.bold {
                            Weight::Bold
                        } else {
                            Weight::Normal
                        },
                        style: if run.italic {
                            Style::Italic
                        } else {
                            Style::Normal
                        },
                    });
                highlights.push(HighlightedRange {
                    highlight,
                    highlight_indices: (start..end).collect(),
                });
            }
            if text.is_empty() {
                text.push(' ');
            }
            screen.add_child(
                Text::new(
                    text,
                    appearance.monospace_font_family(),
                    appearance.monospace_font_size(),
                )
                .with_color(palette.foreground)
                .with_highlights(highlights)
                .with_clip(ClipConfig::end())
                .with_selectable(true)
                .finish(),
            );
        }
        let screen = Container::new(screen.finish())
            .with_uniform_padding(8.)
            .with_background(Fill::Solid(palette.background))
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
                ctx.dispatch_typed_action(RecordingsViewAction::SeekToFraction(value));
            })
            .build()
            .finish();
        let has_chapters = !playback.chapters.is_empty();
        let mut control_buttons = vec![
            button(
                appearance,
                play_label,
                ButtonVariant::Accent,
                &controls.play_pause,
                RecordingsViewAction::TogglePlayPause,
            ),
            button(
                appearance,
                "Restart",
                ButtonVariant::Secondary,
                &controls.restart,
                RecordingsViewAction::Restart,
            ),
            button(
                appearance,
                "-5s",
                ButtonVariant::Secondary,
                &controls.back,
                RecordingsViewAction::SeekBy(-SEEK_STEP_SECONDS),
            ),
            button(
                appearance,
                "+5s",
                ButtonVariant::Secondary,
                &controls.forward,
                RecordingsViewAction::SeekBy(SEEK_STEP_SECONDS),
            ),
        ];
        if has_chapters {
            control_buttons.extend([
                button(
                    appearance,
                    "Previous command",
                    ButtonVariant::Secondary,
                    &controls.previous,
                    RecordingsViewAction::PreviousChapter,
                ),
                button(
                    appearance,
                    "Next command",
                    ButtonVariant::Secondary,
                    &controls.next,
                    RecordingsViewAction::NextChapter,
                ),
            ]);
        }
        control_buttons.extend([
            button(
                appearance,
                &format!("{}x", player.speed()),
                ButtonVariant::Secondary,
                &controls.speed,
                RecordingsViewAction::CycleSpeed,
            ),
            button(
                appearance,
                if player.skip_idle() {
                    "Skip idle: on"
                } else {
                    "Skip idle: off"
                },
                ButtonVariant::Secondary,
                &controls.skip_idle,
                RecordingsViewAction::ToggleSkipIdle,
            ),
        ]);
        let control_row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.)
            .with_children(control_buttons)
            .finish();
        let seek_row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.)
            .with_child(slider)
            .with_child(sub_text(
                appearance,
                format!(
                    "{} / {}",
                    format_duration(player.position()),
                    format_duration(player.duration())
                ),
            ))
            .with_child(button(
                appearance,
                "Close player",
                ButtonVariant::Secondary,
                &controls.close,
                RecordingsViewAction::ClosePlayer,
            ))
            .finish();

        let mut column = Flex::column()
            .with_spacing(8.)
            .with_child(
                Text::new(
                    format!("Playing: {}", playback.name),
                    appearance.ui_font_family(),
                    appearance.ui_font_size() + 1.,
                )
                .with_color(theme.main_text_color(theme.background()).into())
                .finish(),
            )
            .with_child(screen)
            .with_child(control_row)
            .with_child(seek_row);
        if has_chapters {
            column.add_child(self.render_chapters(playback, appearance));
        }
        column.finish()
    }

    fn render_chapters(
        &self,
        playback: &PlaybackSession,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let position = playback.player.position();
        let current = playback
            .chapters
            .iter()
            .rposition(|chapter| chapter.time <= position + 1e-6);
        let mut list = Flex::column().with_spacing(2.);
        for (index, (chapter, mouse_state)) in playback
            .chapters
            .iter()
            .zip(&playback.chapter_mouse_states)
            .enumerate()
        {
            let variant = if current == Some(index) {
                ButtonVariant::Accent
            } else {
                ButtonVariant::Text
            };
            list.add_child(button(
                appearance,
                &format!("{}  {}", format_duration(chapter.time), chapter.label),
                variant,
                mouse_state,
                RecordingsViewAction::SeekTo(chapter.time),
            ));
        }
        Flex::column()
            .with_spacing(4.)
            .with_child(sub_text(appearance, "Commands (n / p to jump)".to_owned()))
            .with_child(
                ConstrainedBox::new(
                    ClippedScrollable::vertical(
                        playback.controls.chapters_scroll.clone(),
                        list.finish(),
                        ScrollbarWidth::Auto,
                        theme.disabled_text_color(theme.background()).into(),
                        theme.main_text_color(theme.background()).into(),
                        Fill::None,
                    )
                    .finish(),
                )
                .with_max_height(CHAPTERS_HEIGHT)
                .finish(),
            )
            .finish()
    }
}

impl Entity for RecordingsView {
    type Event = RecordingsViewEvent;
}

impl View for RecordingsView {
    fn ui_name() -> &'static str {
        "RecordingsView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let mut column = Flex::column()
            .with_spacing(12.)
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(self.render_toolbar(appearance));
        match &self.playback {
            Some(playback) => {
                column.add_child(
                    ConstrainedBox::new(self.render_list(appearance))
                        .with_max_height(LIST_HEIGHT_WITH_PLAYER)
                        .finish(),
                );
                column.add_child(
                    Expanded::new(1., self.render_player(playback, appearance)).finish(),
                );
            }
            None => column.add_child(Expanded::new(1., self.render_list(appearance)).finish()),
        }
        Container::new(column.finish())
            .with_uniform_padding(16.)
            .finish()
    }
}

impl TypedActionView for RecordingsView {
    type Action = RecordingsViewAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            RecordingsViewAction::Refresh => self.refresh(ctx),
            RecordingsViewAction::OpenSettings => ctx.dispatch_typed_action(
                &WorkspaceAction::ShowSettingsPage(SettingsSection::Recordings),
            ),
            RecordingsViewAction::Play(index) => self.play(*index, ctx),
            RecordingsViewAction::Reveal(index) => {
                if let Some(path) = self
                    .list
                    .row(*index)
                    .and_then(|info| info.local_path.clone())
                {
                    ctx.dispatch_typed_action(&WorkspaceAction::OpenInExplorer { path });
                }
            }
            RecordingsViewAction::Copy(index) => self.copy(*index, ctx),
            RecordingsViewAction::StartRename(index) => self.start_rename(*index, ctx),
            RecordingsViewAction::Delete(index) => self.delete(*index, ctx),
            RecordingsViewAction::TogglePlayPause => {
                self.update_player(ctx, Player::toggle_play_pause)
            }
            RecordingsViewAction::Restart => self.update_player(ctx, Player::restart),
            RecordingsViewAction::SeekBy(delta) => {
                let delta = *delta;
                self.update_player(ctx, |player| player.seek(player.position() + delta))
            }
            RecordingsViewAction::SeekToFraction(fraction) => {
                let fraction = *fraction;
                self.update_player(ctx, |player| player.seek_to_fraction(fraction))
            }
            RecordingsViewAction::SeekTo(time) => {
                let time = *time;
                self.update_player(ctx, |player| player.seek(time))
            }
            RecordingsViewAction::NextChapter => self.update_player(ctx, |player| {
                player.next_chapter();
            }),
            RecordingsViewAction::PreviousChapter => {
                self.update_player(ctx, Player::previous_chapter)
            }
            RecordingsViewAction::CycleSpeed => self.update_player(ctx, Player::cycle_speed),
            RecordingsViewAction::ToggleSkipIdle => {
                self.update_player(ctx, |player| player.set_skip_idle(!player.skip_idle()))
            }
            RecordingsViewAction::ClosePlayer => {
                self.playback = None;
                ctx.notify();
            }
        }
    }
}

impl BackingView for RecordingsView {
    type PaneHeaderOverflowMenuAction = RecordingsViewAction;
    type CustomAction = ();
    type AssociatedData = ();

    fn handle_pane_header_overflow_menu_action(
        &mut self,
        action: &Self::PaneHeaderOverflowMenuAction,
        ctx: &mut ViewContext<Self>,
    ) {
        self.handle_action(action, ctx);
    }

    fn handle_custom_action(
        &mut self,
        _custom_action: &Self::CustomAction,
        _ctx: &mut ViewContext<Self>,
    ) {
    }

    fn close(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.emit(RecordingsViewEvent::Pane(PaneEvent::Close));
    }

    fn focus_contents(&mut self, ctx: &mut ViewContext<Self>) {
        self.focus(ctx);
    }

    fn render_header_content(
        &self,
        _ctx: &view::HeaderRenderContext<'_>,
        _app: &AppContext,
    ) -> HeaderContent {
        HeaderContent::Standard(StandardHeader {
            title: HEADER_TEXT.to_owned(),
            title_secondary: None,
            title_style: None,
            title_clip_config: ClipConfig::start(),
            title_max_width: None,
            left_of_title: None,
            right_of_title: None,
            left_of_overflow: None,
            options: StandardHeaderOptions::default(),
        })
    }

    fn set_focus_handle(&mut self, focus_handle: PaneFocusHandle, _ctx: &mut ViewContext<Self>) {
        self.focus_handle = Some(focus_handle);
    }
}

/// The current theme's terminal colors.
fn palette(appearance: &Appearance) -> Palette {
    let theme = appearance.theme();
    let colors = theme.terminal_colors();
    let ansi = |set: &warp_core::ui::theme::AnsiColors| -> [ColorU; 8] {
        [
            set.black.into(),
            set.red.into(),
            set.green.into(),
            set.yellow.into(),
            set.blue.into(),
            set.magenta.into(),
            set.cyan.into(),
            set.white.into(),
        ]
    };
    let mut all = [ColorU::black(); 16];
    all[..8].copy_from_slice(&ansi(&colors.normal));
    all[8..].copy_from_slice(&ansi(&colors.bright));
    Palette {
        foreground: theme.foreground().into(),
        background: theme.background().into(),
        ansi: all,
    }
}

fn button(
    appearance: &Appearance,
    label: &str,
    variant: ButtonVariant,
    mouse_state: &MouseStateHandle,
    action: RecordingsViewAction,
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
