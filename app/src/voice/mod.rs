#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
pub mod context_correction;
#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
pub mod conversation;
#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
mod conversation_text;
#[cfg(not(target_family = "wasm"))]
pub mod disfluency;
#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
pub mod footer_controls;
#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
pub mod hands_free;
#[cfg(not(target_family = "wasm"))]
#[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
pub mod hands_free_controller;
#[cfg(not(target_family = "wasm"))]
pub mod local_model;
#[cfg(not(target_family = "wasm"))]
#[cfg_attr(not(feature = "voice_input"), allow(dead_code))]
mod speaker_gate;
#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
pub mod speaker_id;
#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
pub mod title_bar_controls;
pub mod transcriber;

/// Where macOS lets the user allow system audio recording.
#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
pub(crate) const SYSTEM_AUDIO_PRIVACY_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

/// Applies the system echo cancellation setting to the audio pipeline. Its noise suppression only
/// runs when the RNNoise denoiser is off.
#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
pub fn apply_system_echo_cancellation_setting(app: &warpui::AppContext) {
    use warpui::SingletonEntity as _;

    let settings = crate::settings::AISettings::as_ref(app);
    voice_input::set_system_echo_cancellation(
        *settings.system_echo_cancellation,
        !*settings.voice_denoise,
    );
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
