//! The window's voice session controls at the top right of the tab bar: hands-free input with its
//! dictation mode menu, voice conversation (a waveform that follows the microphone level), the
//! readout's Stop and Mute, and the Voice Isolation hint. One session serves the whole app, so
//! these replace any per-input state.

use std::time::Duration;

use local_transcription::speech::playback::ConversationState;
use pathfinder_color::ColorU;
use pathfinder_geometry::vector::vec2f;
use settings::Setting as _;
use warp_core::ui::theme::Fill;
use warpui::r#async::Timer;
use warpui::elements::{
    Border, ChildAnchor, ChildView, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment,
    Flex, Hoverable, MainAxisSize, MouseStateHandle, OffsetPositioning, ParentAnchor,
    ParentElement, ParentOffsetBounds, PositionedElementAnchor, PositionedElementOffsetBounds,
    Radius, SavePosition, Stack, Text,
};
use warpui::platform::Cursor;
use warpui::ui_components::components::UiComponent as _;
use warpui::{
    AppContext, Element, Entity, SingletonEntity as _, TypedActionView, View, ViewContext,
    ViewHandle,
};

use super::conversation::{ConversationSession, toggle_conversation};
use super::footer_controls::{CONVERSATION_BINDING, DictationMode, HANDS_FREE_BINDING};
use super::hands_free::{HandsFreeSession, toggle_hands_free};
use super::hands_free_controller::HandsFreeState;
use super::local_model::{is_conversation_enabled, is_hands_free_enabled};
use crate::appearance::Appearance;
use crate::menu::{Event as MenuEvent, Menu, MenuItem, MenuItemFields, MenuVariant};
use crate::settings::AISettings;
use crate::ui_components::icons::Icon;
use crate::view_components::action_button::{
    ActionButton, ActionButtonTheme, ButtonSize, KeystrokeSource, NakedTheme, TooltipAlignment,
};

/// How often the waveform follows the microphone level while the microphone is live.
const LEVEL_REFRESH: Duration = Duration::from_millis(40);
/// Relative heights of the waveform's bars at full level, so speech reads as a waveform.
const BAR_WEIGHTS: [f32; 7] = [0.3, 0.6, 0.85, 1.0, 0.85, 0.6, 0.3];
const BAR_WIDTH: f32 = 2.;
const BAR_GAP: f32 = 2.;
const BAR_MIN_HEIGHT: f32 = 2.;
const BAR_MAX_HEIGHT: f32 = 16.;
/// The share of each bar's range shown with no sound, so the idle icon still reads as a waveform
/// instead of a row of dots.
const BAR_REST_LEVEL: f32 = 0.6;
/// Level changes smaller than this don't redraw the waveform.
const LEVEL_EPSILON: f32 = 0.02;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonStatus {
    /// Whether the microphone or conversation is live, shown in red.
    pub live: bool,
    pub tooltip: String,
}

pub fn hands_free_status(state: HandsFreeState) -> ButtonStatus {
    let tooltip = match state {
        HandsFreeState::Off => "Start hands-free voice input".to_owned(),
        HandsFreeState::Paused => {
            "Hands-free paused while Warp isn't active \u{b7} Stop".to_owned()
        }
        HandsFreeState::Listening | HandsFreeState::Hearing | HandsFreeState::Transcribing => {
            format!("Hands-free: {} \u{b7} Stop", state.label())
        }
    };
    ButtonStatus {
        live: state.is_mic_on(),
        tooltip,
    }
}

pub fn conversation_status(active: bool, state: ConversationState, muted: bool) -> ButtonStatus {
    if !active {
        return ButtonStatus {
            live: false,
            tooltip: "Start voice conversation".to_owned(),
        };
    }
    let state_label = match state {
        ConversationState::Off => "Starting",
        ConversationState::Listening
        | ConversationState::Thinking
        | ConversationState::Speaking => state.label(),
    };
    let muted = if muted { ", muted" } else { "" };
    ButtonStatus {
        live: true,
        tooltip: format!("Voice conversation: {state_label}{muted} \u{b7} Stop"),
    }
}

/// The waveform's bar heights for a microphone level in `[0, 1]`; flat while not live.
pub fn waveform_bar_heights(level: f32, live: bool) -> [f32; 7] {
    let level = if live { level.clamp(0., 1.) } else { 0. };
    let level = BAR_REST_LEVEL + (1. - BAR_REST_LEVEL) * level;
    BAR_WEIGHTS.map(|weight| BAR_MIN_HEIGHT + (BAR_MAX_HEIGHT - BAR_MIN_HEIGHT) * weight * level)
}

/// Draws a button's icon and label in red while the microphone or conversation is live.
struct LiveVoiceTheme;

impl ActionButtonTheme for LiveVoiceTheme {
    fn background(&self, hovered: bool, appearance: &Appearance) -> Option<Fill> {
        NakedTheme.background(hovered, appearance)
    }

    fn text_color(
        &self,
        _hovered: bool,
        _background: Option<Fill>,
        appearance: &Appearance,
    ) -> ColorU {
        appearance.theme().ansi_fg_red()
    }

    fn border(&self, appearance: &Appearance) -> Option<ColorU> {
        NakedTheme.border(appearance)
    }

    fn should_opt_out_of_contrast_adjustment(&self) -> bool {
        true
    }
}

fn set_live_theme(live: bool, button: &mut ActionButton, ctx: &mut ViewContext<ActionButton>) {
    if live {
        button.set_theme(LiveVoiceTheme, ctx);
    } else {
        button.set_theme(NakedTheme, ctx);
    }
}

fn new_button() -> ActionButton {
    ActionButton::new("", NakedTheme)
        .with_size(ButtonSize::Small)
        .with_tooltip_alignment(TooltipAlignment::Right)
}

#[derive(Debug, Clone)]
pub enum VoiceTitleBarAction {
    ToggleHandsFree,
    ToggleModeMenu,
    SetDictationMode(DictationMode),
    ToggleConversation,
    StopSpeaking,
    ToggleMute,
    ChooseMicrophoneMode,
    DismissVoiceIsolationHint,
}

pub struct VoiceTitleBarControls {
    hands_free_button: ViewHandle<ActionButton>,
    mode_button: ViewHandle<ActionButton>,
    mode_menu: ViewHandle<Menu<VoiceTitleBarAction>>,
    mode_menu_open: bool,
    conversation_mouse_state: MouseStateHandle,
    conversation: ButtonStatus,
    stop_button: ViewHandle<ActionButton>,
    mute_button: ViewHandle<ActionButton>,
    microphone_mode_button: ViewHandle<ActionButton>,
    dismiss_voice_isolation_button: ViewHandle<ActionButton>,
    show_voice_isolation_hint: bool,
    /// The microphone level the waveform shows.
    level: f32,
    /// Whether a level refresh timer is running; it runs only while the microphone is live.
    level_ticking: bool,
}

impl VoiceTitleBarControls {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let hands_free_button = ctx.add_typed_action_view(|_| {
            new_button()
                .with_icon(Icon::Microphone)
                .on_click(|ctx| ctx.dispatch_typed_action(VoiceTitleBarAction::ToggleHandsFree))
        });
        let mode_button = ctx.add_typed_action_view(|_| {
            new_button()
                .with_icon(Icon::ChevronDown)
                .with_tooltip("Dictation mode")
                .on_click(|ctx| ctx.dispatch_typed_action(VoiceTitleBarAction::ToggleModeMenu))
        });
        let mode_menu = ctx.add_typed_action_view(|ctx| {
            let theme = Appearance::as_ref(ctx).theme();
            Menu::new()
                .with_menu_variant(MenuVariant::Fixed)
                .with_border(Border::all(1.).with_border_fill(theme.outline()))
                .with_drop_shadow()
        });
        ctx.subscribe_to_view(&mode_menu, |me, _, event, ctx| match event {
            MenuEvent::Close { .. } => {
                me.mode_menu_open = false;
                ctx.notify();
            }
            MenuEvent::ItemSelected | MenuEvent::ItemHovered => {}
        });
        let stop_button = ctx.add_typed_action_view(|_| {
            new_button()
                .with_icon(Icon::StopFilled)
                .with_tooltip("Stop reading the reply")
                .on_click(|ctx| ctx.dispatch_typed_action(VoiceTitleBarAction::StopSpeaking))
        });
        let mute_button = ctx.add_typed_action_view(|_| {
            new_button().on_click(|ctx| ctx.dispatch_typed_action(VoiceTitleBarAction::ToggleMute))
        });
        let microphone_mode_button = ctx.add_typed_action_view(|_| {
            new_button()
                .with_tooltip("Choose the macOS microphone mode")
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(VoiceTitleBarAction::ChooseMicrophoneMode)
                })
        });
        microphone_mode_button.update(ctx, |button, ctx| {
            button.set_label("Turn on Voice Isolation", ctx)
        });
        let dismiss_voice_isolation_button = ctx.add_typed_action_view(|_| {
            new_button()
                .with_icon(Icon::X)
                .with_tooltip("Don't suggest Voice Isolation again")
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(VoiceTitleBarAction::DismissVoiceIsolationHint)
                })
        });
        ctx.spawn_stream_local(
            voice_input::microphone_mode::changes(),
            |me, (), ctx| me.refresh(ctx),
            |_, _| {},
        );
        ctx.spawn_stream_local(
            voice_input::backend_changes(),
            |me, (), ctx| me.refresh(ctx),
            |_, _| {},
        );
        ctx.subscribe_to_model(&HandsFreeSession::handle(ctx), |me, _, _, ctx| {
            me.refresh(ctx)
        });
        ctx.subscribe_to_model(&ConversationSession::handle(ctx), |me, _, _, ctx| {
            me.refresh(ctx)
        });

        let mut me = Self {
            hands_free_button,
            mode_button,
            mode_menu,
            mode_menu_open: false,
            conversation_mouse_state: MouseStateHandle::default(),
            conversation: conversation_status(false, ConversationState::Off, false),
            stop_button,
            mute_button,
            microphone_mode_button,
            dismiss_voice_isolation_button,
            show_voice_isolation_hint: false,
            level: 0.,
            level_ticking: false,
        };
        me.refresh(ctx);
        me
    }

    /// Whether anything shows: the controls only exist when a voice mode is available.
    pub fn is_available() -> bool {
        is_hands_free_enabled() || is_conversation_enabled()
    }

    fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        let session = HandsFreeSession::as_ref(ctx);
        let state = session.state();
        let mode = session.dictation_mode();
        let status = hands_free_status(state);
        let sublabel = KeystrokeSource::Binding(HANDS_FREE_BINDING).displayed(ctx);
        self.hands_free_button.update(ctx, |button, ctx| {
            set_live_theme(status.live, button, ctx);
            button.set_active(status.live, ctx);
            button.set_tooltip(Some(status.tooltip), ctx);
            button.set_tooltip_sublabel(sublabel, ctx);
        });
        self.mode_button.update(ctx, |button, ctx| {
            set_live_theme(status.live, button, ctx);
        });

        let conversation = ConversationSession::as_ref(ctx);
        self.conversation = conversation_status(
            conversation.is_active(),
            conversation.state(),
            conversation.is_muted(),
        );
        let mute_label = if conversation.is_muted() {
            "Unmute"
        } else {
            "Mute"
        };
        let mute_tooltip = if conversation.is_muted() {
            "Read replies aloud again"
        } else {
            "Stop reading replies aloud; conversation stays on"
        };
        self.mute_button.update(ctx, |button, ctx| {
            button.set_label(mute_label, ctx);
            button.set_tooltip(Some(mute_tooltip), ctx);
        });

        self.show_voice_isolation_hint = state.is_mic_on()
            && voice_input::microphone_mode::should_suggest_voice_isolation(
                voice_input::microphone_mode::microphone_mode(),
                voice_input::microphone_mode::capture_backend(),
                *AISettings::as_ref(ctx).voice_isolation_hint_dismissed,
            );

        let items = mode_menu_items(mode);
        self.mode_menu
            .update(ctx, |menu, ctx| menu.set_items(items, ctx));
        self.follow_level(ctx);
        ctx.notify();
    }

    /// Polls the microphone level while the microphone is live; stops once it isn't, so an idle
    /// window has no redraw loop.
    fn follow_level(&mut self, ctx: &mut ViewContext<Self>) {
        let session = HandsFreeSession::as_ref(ctx);
        let live = session.state().is_mic_on() && is_conversation_enabled();
        let level = if live { session.mic_level() } else { 0. };
        if (level - self.level).abs() >= LEVEL_EPSILON || (level == 0. && self.level != 0.) {
            self.level = level;
            ctx.notify();
        }
        if !live || self.level_ticking {
            return;
        }
        self.level_ticking = true;
        ctx.spawn(Timer::after(LEVEL_REFRESH), |me, _, ctx| {
            me.level_ticking = false;
            me.follow_level(ctx);
        });
    }

    fn render_mode_button(&self) -> Box<dyn Element> {
        let position_id = format!("voice_title_bar::mode_button_{}", self.mode_button.id());
        let mut stack = Stack::new().with_child(
            SavePosition::new(ChildView::new(&self.mode_button).finish(), &position_id).finish(),
        );
        if self.mode_menu_open {
            stack.add_positioned_overlay_child(
                ChildView::new(&self.mode_menu).finish(),
                OffsetPositioning::offset_from_save_position_element(
                    position_id,
                    vec2f(0., 4.),
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::BottomRight,
                    ChildAnchor::TopRight,
                ),
            );
        }
        stack.finish()
    }

    /// The conversation toggle: a waveform whose bars follow the microphone level.
    fn render_conversation_button(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let status = self.conversation.clone();
        let color = if status.live {
            appearance.theme().ansi_fg_red()
        } else {
            appearance.theme().foreground().into()
        };
        let heights = waveform_bar_heights(self.level, status.live);
        let ui_builder = appearance.ui_builder().clone();
        let hover_fill = NakedTheme.background(true, appearance);
        let sublabel = KeystrokeSource::Binding(CONVERSATION_BINDING).displayed(app);
        Hoverable::new(self.conversation_mouse_state.clone(), move |state| {
            let mut bars = Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center);
            for (index, height) in heights.iter().enumerate() {
                let bar = ConstrainedBox::new(
                    Container::new(Flex::row().finish())
                        .with_background_color(color)
                        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(1.)))
                        .finish(),
                )
                .with_width(BAR_WIDTH)
                .with_height(*height)
                .finish();
                let margin = if index + 1 < heights.len() {
                    BAR_GAP
                } else {
                    0.
                };
                bars.add_child(Container::new(bar).with_margin_right(margin).finish());
            }
            let mut container = Container::new(
                ConstrainedBox::new(bars.finish())
                    .with_height(BAR_MAX_HEIGHT)
                    .finish(),
            )
            .with_uniform_padding(4.)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)));
            if state.is_hovered()
                && let Some(fill) = hover_fill
            {
                container = container.with_background(fill);
            }
            let mut stack = Stack::new().with_child(container.finish());
            if state.is_hovered() {
                let label = match &sublabel {
                    Some(keys) => format!("{} ({keys})", status.tooltip),
                    None => status.tooltip.clone(),
                };
                stack.add_positioned_overlay_child(
                    ui_builder.tool_tip(label).build().finish(),
                    OffsetPositioning::offset_from_parent(
                        vec2f(0., 4.),
                        ParentOffsetBounds::WindowByPosition,
                        ParentAnchor::BottomRight,
                        ChildAnchor::TopRight,
                    ),
                );
            }
            stack.finish()
        })
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(VoiceTitleBarAction::ToggleConversation))
        .with_cursor(Cursor::PointingHand)
        .finish()
    }
}

fn mode_menu_items(selected: DictationMode) -> Vec<MenuItem<VoiceTitleBarAction>> {
    [DictationMode::ClickToRecord, DictationMode::HandsFree]
        .into_iter()
        .map(|mode| {
            let item = MenuItemFields::new(mode.menu_label())
                .with_on_select_action(VoiceTitleBarAction::SetDictationMode(mode));
            if mode == selected {
                item.with_icon(Icon::Check)
            } else {
                item.with_indent()
            }
            .into_item()
        })
        .collect()
}

impl Entity for VoiceTitleBarControls {
    type Event = ();
}

impl View for VoiceTitleBarControls {
    fn ui_name() -> &'static str {
        "VoiceTitleBarControls"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);
        let hands_free = HandsFreeSession::as_ref(app);
        if hands_free.showing_rejection() {
            let appearance = Appearance::as_ref(app);
            row.add_child(
                Container::new(
                    Text::new_inline("Not you \u{2014} ignored", appearance.ui_font_family(), 12.)
                        .with_color(appearance.theme().ansi_fg_red())
                        .finish(),
                )
                .with_margin_right(6.)
                .finish(),
            );
        }
        if self.show_voice_isolation_hint {
            row.add_child(ChildView::new(&self.microphone_mode_button).finish());
            row.add_child(ChildView::new(&self.dismiss_voice_isolation_button).finish());
        }
        let conversation = ConversationSession::as_ref(app);
        if conversation.is_active() {
            if conversation.is_speaking() {
                row.add_child(ChildView::new(&self.stop_button).finish());
            }
            row.add_child(ChildView::new(&self.mute_button).finish());
        }
        if is_hands_free_enabled() {
            row.add_child(ChildView::new(&self.hands_free_button).finish());
            row.add_child(self.render_mode_button());
        }
        if is_conversation_enabled() {
            row.add_child(self.render_conversation_button(app));
        }
        row.finish()
    }
}

impl TypedActionView for VoiceTitleBarControls {
    type Action = VoiceTitleBarAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            VoiceTitleBarAction::ToggleHandsFree => toggle_hands_free(ctx.window_id(), ctx),
            VoiceTitleBarAction::ToggleModeMenu => {
                self.mode_menu_open = !self.mode_menu_open;
                ctx.notify();
            }
            VoiceTitleBarAction::SetDictationMode(mode) => {
                self.mode_menu_open = false;
                HandsFreeSession::handle(ctx)
                    .update(ctx, |session, ctx| session.set_dictation_mode(*mode, ctx));
                ctx.notify();
            }
            VoiceTitleBarAction::ToggleConversation => {
                toggle_conversation(ctx.window_id(), None, ctx);
            }
            VoiceTitleBarAction::StopSpeaking => {
                ConversationSession::handle(ctx).update(ctx, |session, ctx| {
                    session.stop_speaking(ctx);
                });
            }
            VoiceTitleBarAction::ToggleMute => {
                ConversationSession::handle(ctx)
                    .update(ctx, |session, ctx| session.toggle_muted(ctx));
            }
            VoiceTitleBarAction::ChooseMicrophoneMode => {
                voice_input::microphone_mode::show_microphone_mode_picker();
            }
            VoiceTitleBarAction::DismissVoiceIsolationHint => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    if let Err(error) = settings.voice_isolation_hint_dismissed.set_value(true, ctx)
                    {
                        log::warn!(
                            "Could not remember the Voice Isolation hint dismissal: {error}"
                        );
                    }
                });
                self.refresh(ctx);
            }
        }
    }
}

#[cfg(test)]
#[path = "title_bar_controls_tests.rs"]
mod tests;
