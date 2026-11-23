//! An input footer's push-to-talk mic: what its click does under the app-wide dictation mode, and
//! its tooltip. The host footer owns the mic button and routes its clicks through
//! [`handle_mic_click`].

use settings::Setting as _;
use warpui::elements::Empty;
use warpui::{
    AppContext, Element, Entity, SingletonEntity as _, View, ViewContext, ViewHandle, WindowId,
};

use super::hands_free::{HandsFreeSession, toggle_hands_free};
use super::local_model::is_hands_free_enabled;
use crate::settings::{AISettings, AISettingsChangedEvent};
use crate::view_components::action_button::{ActionButton, KeystrokeSource};

pub(super) const HANDS_FREE_BINDING: &str = "workspace:enable_hands_free_voice";
pub(super) const CONVERSATION_BINDING: &str = "workspace:enable_voice_conversation";

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
    pub(super) fn menu_label(self) -> &'static str {
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

/// The footer mic's tooltip: what a click does given the mode and whether hands-free is on.
pub fn mic_tooltip(
    mode: DictationMode,
    hands_free_available: bool,
    hands_free_on: bool,
) -> MicTooltip {
    match mic_click(mode, hands_free_available, hands_free_on) {
        MicClick::Record => MicTooltip::Record,
        MicClick::ToggleHandsFree if hands_free_on => MicTooltip::StopHandsFree,
        MicClick::ToggleHandsFree => MicTooltip::StartHandsFree,
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

/// Keeps a footer's push-to-talk mic button's tooltip in step with the app-wide hands-free
/// session and dictation mode. The session controls themselves live in the tab bar.
pub struct VoiceControls {
    /// Owned by the host footer, which also drives its single-recording state.
    mic_button: ViewHandle<ActionButton>,
}

impl VoiceControls {
    pub fn new(mic_button: ViewHandle<ActionButton>, ctx: &mut ViewContext<Self>) -> Self {
        ctx.subscribe_to_model(&HandsFreeSession::handle(ctx), |me, _, _, ctx| {
            me.refresh(ctx)
        });
        ctx.subscribe_to_model(&AISettings::handle(ctx), |me, _, event, ctx| {
            if let AISettingsChangedEvent::VoiceInputToggleKey { .. } = event {
                me.refresh(ctx);
            }
        });
        let me = Self { mic_button };
        me.refresh(ctx);
        me
    }

    fn refresh(&self, ctx: &mut ViewContext<Self>) {
        let session = HandsFreeSession::as_ref(ctx);
        let tooltip = mic_tooltip(
            session.dictation_mode(),
            is_hands_free_enabled(),
            session.is_active(),
        );
        let text = match tooltip {
            MicTooltip::Record => AISettings::as_ref(ctx)
                .voice_input_toggle_key
                .value()
                .tooltip_message(),
            MicTooltip::StartHandsFree => "Start hands-free dictation".to_owned(),
            MicTooltip::StopHandsFree => "Stop hands-free dictation".to_owned(),
        };
        let sublabel = tooltip
            .binding()
            .and_then(|name| KeystrokeSource::Binding(name).displayed(ctx));
        self.mic_button.update(ctx, |button, ctx| {
            button.set_tooltip(Some(text), ctx);
            button.set_tooltip_sublabel(sublabel, ctx);
        });
    }
}

impl Entity for VoiceControls {
    type Event = ();
}

impl View for VoiceControls {
    fn ui_name() -> &'static str {
        "VoiceControls"
    }

    fn render(&self, _: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

#[cfg(test)]
#[path = "footer_controls_tests.rs"]
mod tests;
