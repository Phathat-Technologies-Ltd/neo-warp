#[cfg(not(target_family = "wasm"))]
pub mod local_model;
pub mod transcriber;

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
