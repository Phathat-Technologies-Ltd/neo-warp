//! The voice controls next to an input footer's mic button: a dictation mode dropdown and a
//! voice conversation button. The host footer owns the mic button and routes its clicks through
//! [`handle_mic_click`].

use std::sync::Arc;

use local_transcription::speech::playback::ConversationState;
use pathfinder_color::ColorU;
use pathfinder_geometry::vector::vec2f;
use settings::Setting as _;
use warp_core::ui::theme::Fill;
use warpui::elements::{
    Border, ChildAnchor, ChildView, CrossAxisAlignment, Flex, MainAxisSize, OffsetPositioning,
    ParentElement, PositionedElementAnchor, PositionedElementOffsetBounds, SavePosition, Stack,
};
use warpui::{
    AppContext, Element, Entity, EntityId, SingletonEntity as _, TypedActionView, View,
    ViewContext, ViewHandle, WindowId,
};

use super::conversation::{ConversationSession, toggle_conversation};
use super::hands_free::{HandsFreeSession, toggle_hands_free};
use super::hands_free_controller::HandsFreeState;
use super::local_model::{is_conversation_enabled, is_hands_free_enabled};
use crate::ai::blocklist::agent_view::agent_input_footer::{
    ActiveMicButtonTheme, AgentInputButtonTheme,
};
use crate::ai::blocklist::prompt::PromptIconButtonTheme;
use crate::appearance::Appearance;
use crate::menu::{Event as MenuEvent, Menu, MenuItem, MenuItemFields, MenuVariant};
use crate::settings::{AISettings, AISettingsChangedEvent};
use crate::terminal::input::{MenuPositioning, MenuPositioningProvider};
use crate::ui_components::icons::Icon;
use crate::view_components::action_button::{
    ActionButton, ActionButtonTheme, ButtonSize, KeystrokeSource, TooltipAlignment,
};

const HANDS_FREE_BINDING: &str = "workspace:enable_hands_free_voice";
const CONVERSATION_BINDING: &str = "workspace:enable_voice_conversation";

/// What a click on a footer mic button starts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum DictationMode {
    /// One recording per click, transcribed when it stops.
    #[default]
    ClickToRecord,
    /// Live dictation that stays on and cuts utterances by voice activity.
    HandsFree,
}

impl DictationMode {
    fn menu_label(self) -> &'static str {
        match self {
            DictationMode::ClickToRecord => "Push to talk / click to record",
            DictationMode::HandsFree => "Hands-free",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicClick {
    /// The host's existing single-recording flow.
    Record,
    ToggleHandsFree,
}

/// Which flow a mic click runs. A running hands-free session is always stopped by the mic, so the
/// button doubles as the off switch whatever the selected mode is.
pub fn mic_click(mode: DictationMode, hands_free_available: bool, hands_free_on: bool) -> MicClick {
    if hands_free_on {
        return MicClick::ToggleHandsFree;
    }
    match effective_mode(mode, hands_free_available) {
        DictationMode::ClickToRecord => MicClick::Record,
        DictationMode::HandsFree => MicClick::ToggleHandsFree,
    }
}

fn effective_mode(mode: DictationMode, hands_free_available: bool) -> DictationMode {
    if hands_free_available {
        mode
    } else {
        DictationMode::ClickToRecord
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicTooltip {
    /// The voice input setting's own tooltip, which names the hold-to-talk key.
    Record,
    StartHandsFree,
    StopHandsFree,
}

impl MicTooltip {
    fn binding(self) -> Option<&'static str> {
        match self {
            MicTooltip::Record => None,
            MicTooltip::StartHandsFree | MicTooltip::StopHandsFree => Some(HANDS_FREE_BINDING),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicVisual {
    /// Whether the mic is open for hands-free dictation, shown in red.
    pub live: bool,
    pub tooltip: MicTooltip,
}

pub fn mic_visual(
    mode: DictationMode,
    hands_free_available: bool,
    hands_free_state: HandsFreeState,
) -> MicVisual {
    if hands_free_state.is_mic_on() {
        return MicVisual {
            live: true,
            tooltip: MicTooltip::StopHandsFree,
        };
    }
    let tooltip = match effective_mode(mode, hands_free_available) {
        DictationMode::ClickToRecord => MicTooltip::Record,
        DictationMode::HandsFree => MicTooltip::StartHandsFree,
    };
    MicVisual {
        live: false,
        tooltip,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationVisual {
    pub live: bool,
    /// Shown next to the waveform while conversation mode is on.
    pub label: String,
    pub tooltip: &'static str,
}

pub fn conversation_visual(
    active: bool,
    state: ConversationState,
    muted: bool,
) -> ConversationVisual {
    if !active {
        return ConversationVisual {
            live: false,
            label: String::new(),
            tooltip: "Start voice conversation",
        };
    }
    let state_label = match state {
        ConversationState::Off => "Starting",
        ConversationState::Listening
        | ConversationState::Thinking
        | ConversationState::Speaking => state.label(),
    };
    ConversationVisual {
        live: true,
        label: if muted {
            format!("{state_label} (muted)")
        } else {
            state_label.to_owned()
        },
        tooltip: "Stop voice conversation",
    }
}

/// The hands-free state and conversation activity a footer shows: the live state only in the
/// session's target input; every other footer shows idle buttons.
pub fn visible_voice_state(
    is_target: bool,
    hands_free_state: HandsFreeState,
    conversation_active: bool,
) -> (HandsFreeState, bool) {
    if is_target {
        (hands_free_state, conversation_active)
    } else {
        (HandsFreeState::Off, false)
    }
}

/// Runs a footer mic click. Returns `false` when the host should run its own recording flow.
pub fn handle_mic_click(window_id: WindowId, ctx: &mut AppContext) -> bool {
    let session = HandsFreeSession::as_ref(ctx);
    match mic_click(
        session.dictation_mode(),
        is_hands_free_enabled(),
        session.is_active(),
    ) {
        MicClick::Record => false,
        MicClick::ToggleHandsFree => {
            toggle_hands_free(window_id, ctx);
            true
        }
    }
}

/// The input footer the controls sit in, which sets their button themes and size.
#[derive(Debug, Clone, Copy)]
pub enum VoiceControlsStyle {
    AgentFooter,
    UniversalInput,
}

impl VoiceControlsStyle {
    fn size(self) -> ButtonSize {
        match self {
            VoiceControlsStyle::AgentFooter => ButtonSize::AgentInputButton,
            VoiceControlsStyle::UniversalInput => ButtonSize::UDIButton,
        }
    }

    fn set_mic_theme(
        self,
        live: bool,
        button: &mut ActionButton,
        ctx: &mut ViewContext<ActionButton>,
    ) {
        match (self, live) {
            (VoiceControlsStyle::AgentFooter, false) => button.set_theme(ActiveMicButtonTheme, ctx),
            (VoiceControlsStyle::AgentFooter, true) => {
                button.set_theme(LiveVoiceTheme(ActiveMicButtonTheme), ctx)
            }
            (VoiceControlsStyle::UniversalInput, false) => {
                button.set_theme(PromptIconButtonTheme::new(false), ctx)
            }
            (VoiceControlsStyle::UniversalInput, true) => {
                button.set_theme(LiveVoiceTheme(PromptIconButtonTheme::new(false)), ctx)
            }
        }
    }

    fn set_button_theme(
        self,
        live: bool,
        button: &mut ActionButton,
        ctx: &mut ViewContext<ActionButton>,
    ) {
        match (self, live) {
            (VoiceControlsStyle::AgentFooter, false) => {
                button.set_theme(AgentInputButtonTheme, ctx)
            }
            (VoiceControlsStyle::AgentFooter, true) => {
                button.set_theme(LiveVoiceTheme(AgentInputButtonTheme), ctx)
            }
            (VoiceControlsStyle::UniversalInput, false) => {
                button.set_theme(PromptIconButtonTheme::new(false), ctx)
            }
            (VoiceControlsStyle::UniversalInput, true) => {
                button.set_theme(LiveVoiceTheme(PromptIconButtonTheme::new(false)), ctx)
            }
        }
    }

    fn new_button(self) -> ActionButton {
        let button = match self {
            VoiceControlsStyle::AgentFooter => ActionButton::new("", AgentInputButtonTheme),
            VoiceControlsStyle::UniversalInput => {
                ActionButton::new("", PromptIconButtonTheme::new(false))
            }
        };
        button
            .with_size(self.size())
            .with_tooltip_alignment(TooltipAlignment::Left)
    }
}

/// Draws a button's icon and label in red while the microphone or conversation is live.
struct LiveVoiceTheme<T>(T);

impl<T: ActionButtonTheme> ActionButtonTheme for LiveVoiceTheme<T> {
    fn background(&self, hovered: bool, appearance: &Appearance) -> Option<Fill> {
        self.0.background(hovered, appearance)
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
        self.0.border(appearance)
    }

    fn should_opt_out_of_contrast_adjustment(&self) -> bool {
        true
    }

    fn font_properties(&self) -> Option<warpui::fonts::Properties> {
        self.0.font_properties()
    }
}

#[derive(Debug, Clone)]
pub enum VoiceControlsAction {
    ToggleModeMenu,
    SetDictationMode(DictationMode),
    ToggleConversation,
}

pub struct VoiceControls {
    style: VoiceControlsStyle,
    /// The terminal surface whose input hosts these controls.
    host: EntityId,
    /// Owned by the host footer, which also drives its single-recording state.
    mic_button: ViewHandle<ActionButton>,
    mode_button: ViewHandle<ActionButton>,
    mode_menu: ViewHandle<Menu<VoiceControlsAction>>,
    mode_menu_open: bool,
    conversation_button: ViewHandle<ActionButton>,
    menu_positioning_provider: Arc<dyn MenuPositioningProvider>,
}

impl VoiceControls {
    pub fn new(
        style: VoiceControlsStyle,
        host: EntityId,
        mic_button: ViewHandle<ActionButton>,
        menu_positioning_provider: Arc<dyn MenuPositioningProvider>,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let mode_button = ctx.add_typed_action_view(|_| {
            style
                .new_button()
                .with_icon(Icon::ChevronDown)
                .with_tooltip("Dictation mode")
                .on_click(|ctx| ctx.dispatch_typed_action(VoiceControlsAction::ToggleModeMenu))
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
        let conversation_button = ctx.add_typed_action_view(|_| {
            style
                .new_button()
                .with_icon(Icon::AudioWaveform)
                .with_tooltip_keybinding(CONVERSATION_BINDING)
                .on_click(|ctx| ctx.dispatch_typed_action(VoiceControlsAction::ToggleConversation))
        });

        ctx.subscribe_to_model(&HandsFreeSession::handle(ctx), |me, _, _, ctx| {
            me.refresh(ctx)
        });
        ctx.subscribe_to_model(&ConversationSession::handle(ctx), |me, _, _, ctx| {
            me.refresh(ctx)
        });
        ctx.subscribe_to_model(&AISettings::handle(ctx), |me, _, event, ctx| {
            if let AISettingsChangedEvent::VoiceInputToggleKey { .. } = event {
                me.refresh(ctx);
            }
        });

        let mut me = Self {
            style,
            host,
            mic_button,
            mode_button,
            mode_menu,
            mode_menu_open: false,
            conversation_button,
            menu_positioning_provider,
        };
        me.refresh(ctx);
        me
    }

    fn mode_button_position_id(&self, view_id: impl std::fmt::Display) -> String {
        format!("voice_controls::mode_button_{view_id}")
    }

    fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        let session = HandsFreeSession::as_ref(ctx);
        let mode = session.dictation_mode();
        let conversation = ConversationSession::as_ref(ctx);
        let (hands_free_state, conversation_active) = visible_voice_state(
            session.is_target(ctx.window_id(), self.host),
            session.state(),
            conversation.is_active(),
        );
        let mic = mic_visual(mode, is_hands_free_enabled(), hands_free_state);
        let record_tooltip = AISettings::as_ref(ctx)
            .voice_input_toggle_key
            .value()
            .tooltip_message();
        let tooltip = match mic.tooltip {
            MicTooltip::Record => record_tooltip,
            MicTooltip::StartHandsFree => "Start hands-free dictation".to_owned(),
            MicTooltip::StopHandsFree => "Stop hands-free dictation".to_owned(),
        };
        let sublabel = mic
            .tooltip
            .binding()
            .and_then(|name| KeystrokeSource::Binding(name).displayed(ctx));
        let style = self.style;
        self.mic_button.update(ctx, |button, ctx| {
            style.set_mic_theme(mic.live, button, ctx);
            button.set_tooltip(Some(tooltip), ctx);
            button.set_tooltip_sublabel(sublabel, ctx);
        });
        self.mode_button.update(ctx, |button, ctx| {
            style.set_button_theme(mic.live, button, ctx);
        });

        let conversation = ConversationSession::as_ref(ctx);
        let visual = conversation_visual(
            conversation_active,
            conversation.state(),
            conversation.is_muted(),
        );
        self.conversation_button.update(ctx, |button, ctx| {
            style.set_button_theme(visual.live, button, ctx);
            button.set_active(visual.live, ctx);
            button.set_label(visual.label, ctx);
            button.set_tooltip(Some(visual.tooltip), ctx);
        });

        let items = mode_menu_items(mode);
        self.mode_menu
            .update(ctx, |menu, ctx| menu.set_items(items, ctx));
        ctx.notify();
    }

    fn render_mode_button(&self, app: &AppContext, position_id: String) -> Box<dyn Element> {
        let mut stack = Stack::new().with_child(
            SavePosition::new(ChildView::new(&self.mode_button).finish(), &position_id).finish(),
        );
        if self.mode_menu_open {
            let positioning = match self.menu_positioning_provider.menu_position(app) {
                MenuPositioning::BelowInputBox => {
                    OffsetPositioning::offset_from_save_position_element(
                        position_id,
                        vec2f(0., 4.),
                        PositionedElementOffsetBounds::WindowByPosition,
                        PositionedElementAnchor::BottomLeft,
                        ChildAnchor::TopLeft,
                    )
                }
                MenuPositioning::AboveInputBox => {
                    OffsetPositioning::offset_from_save_position_element(
                        position_id,
                        vec2f(0., -4.),
                        PositionedElementOffsetBounds::WindowByPosition,
                        PositionedElementAnchor::TopLeft,
                        ChildAnchor::BottomLeft,
                    )
                }
            };
            stack.add_positioned_overlay_child(
                ChildView::new(&self.mode_menu).finish(),
                positioning,
            );
        }
        stack.finish()
    }
}

fn mode_menu_items(selected: DictationMode) -> Vec<MenuItem<VoiceControlsAction>> {
    [DictationMode::ClickToRecord, DictationMode::HandsFree]
        .into_iter()
        .map(|mode| {
            let item = MenuItemFields::new(mode.menu_label())
                .with_on_select_action(VoiceControlsAction::SetDictationMode(mode));
            if mode == selected {
                item.with_icon(Icon::Check)
            } else {
                item.with_indent()
            }
            .into_item()
        })
        .collect()
}

impl Entity for VoiceControls {
    type Event = ();
}

impl View for VoiceControls {
    fn ui_name() -> &'static str {
        "VoiceControls"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);
        if is_hands_free_enabled() {
            let position_id = self.mode_button_position_id(self.mode_button.id());
            row = row.with_child(self.render_mode_button(app, position_id));
        }
        if is_conversation_enabled() {
            row = row.with_child(ChildView::new(&self.conversation_button).finish());
        }
        row.finish()
    }
}

impl TypedActionView for VoiceControls {
    type Action = VoiceControlsAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            VoiceControlsAction::ToggleModeMenu => {
                self.mode_menu_open = !self.mode_menu_open;
                ctx.notify();
            }
            VoiceControlsAction::SetDictationMode(mode) => {
                self.mode_menu_open = false;
                HandsFreeSession::handle(ctx)
                    .update(ctx, |session, ctx| session.set_dictation_mode(*mode, ctx));
                ctx.notify();
            }
            VoiceControlsAction::ToggleConversation => {
                toggle_conversation(ctx.window_id(), ctx);
            }
        }
    }
}

#[cfg(test)]
#[path = "footer_controls_tests.rs"]
mod tests;
