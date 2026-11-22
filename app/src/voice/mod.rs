#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
pub mod conversation;
#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
mod conversation_text;
#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
pub mod hands_free;
#[cfg(not(target_family = "wasm"))]
#[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
pub mod hands_free_controller;
#[cfg(not(target_family = "wasm"))]
pub mod local_model;
pub mod transcriber;

/// The window-level privacy indicator: a red dot and "Mic on" while hands-free input has the
/// microphone open, plus the conversation state when conversation mode is on.
#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
pub fn mic_indicator_label(app: &warpui::AppContext) -> Option<String> {
    use warpui::SingletonEntity as _;

    let session = hands_free::HandsFreeSession::as_ref(app);
    if !session.is_active() {
        return None;
    }
    if !session.state().is_mic_on() {
        return Some(session.state().label().to_owned());
    }
    let conversation = conversation::ConversationSession::as_ref(app);
    Some(if conversation.is_active() {
        let state = if conversation.is_muted() {
            format!("{} (muted)", conversation.state().label())
        } else {
            conversation.state().label().to_owned()
        };
        format!("\u{25cf} Mic on \u{b7} {state}")
    } else {
        "\u{25cf} Mic on".to_owned()
    })
}

/// Whether voice input is transcribed on this machine, which has no Warp voice quota.
pub fn is_voice_transcribed_locally() -> bool {
    #[cfg(not(target_family = "wasm"))]
    {
        local_model::is_local_voice_enabled()
    }
    #[cfg(target_family = "wasm")]
    {
        false
    }
}
