pub mod ai_agent_tasks;
pub mod billing_unit;
pub mod settings;
pub mod terminal;
mod virtual_fs;

pub use terminal::add_window_with_terminal;
pub use virtual_fs::{Stub, VirtualFS};
pub use warp_terminal::test_util::mock_blockgrid;

macro_rules! assert_eventually {
    ($cond:expr_2021, $($arg:tt)+) => {
        $crate::test_util::assert_eventually!(20 => $cond, $($arg)+);
    };
    // Run the condition up to ticks times, yielding to the executor in between.  If it does
    // not become true, this panics with the provided format string + args.
    ($ticks:literal => $cond:expr_2021, $($arg:tt)+) => {{
        let mut pass = false;
        for _ in 0..$ticks {
            if $cond {
                pass = true;
                break;
            }
            warpui::r#async::Timer::after(std::time::Duration::from_millis(5)).await;
        }
        if !pass {
            panic!("{}", format_args!($($arg)+));
        }
    }};
}
pub(crate) use assert_eventually;

/// Registers the voice models views read at construction, as the app does at startup. Skips any
/// a test already registered.
/// Without voice input there are no voice models to register.
#[cfg(not(all(not(target_family = "wasm"), feature = "voice_input")))]
pub fn register_voice_models_for_tests(_app: &mut warpui::App) {}

#[cfg(all(not(target_family = "wasm"), feature = "voice_input"))]
pub fn register_voice_models_for_tests(app: &mut warpui::App) {
    use crate::voice;
    use warpui::SingletonEntity as _;
    app.update(|ctx| {
        if !ctx.has_singleton_model::<voice::local_model::LocalVoiceModel>() {
            ctx.add_singleton_model(|_| voice::local_model::LocalVoiceModel::new());
        }
        if !ctx.has_singleton_model::<voice::local_model::SpeakerIdModel>() {
            ctx.add_singleton_model(|_| voice::local_model::SpeakerIdModel::new());
        }
        if !ctx.has_singleton_model::<voice::local_model::AcceleratedSpeechModel>() {
            ctx.add_singleton_model(|_| voice::local_model::AcceleratedSpeechModel::new());
        }
        if !ctx.has_singleton_model::<voice::speaker_id::SpeakerProfile>() {
            ctx.add_singleton_model(|_| voice::speaker_id::SpeakerProfile::new());
        }
        if !ctx.has_singleton_model::<voice::hands_free::HandsFreeSession>() {
            ctx.add_singleton_model(|_| voice::hands_free::HandsFreeSession::default());
        }
        if !ctx.has_singleton_model::<voice::context_correction::VoiceCorrections>() {
            ctx.add_singleton_model(voice::context_correction::VoiceCorrections::new);
        }
        if !ctx.has_singleton_model::<voice::local_model::SpeechOutputModel>() {
            ctx.add_singleton_model(|ctx| {
                let engine = voice::local_model::SpeechOutputEngine::from_setting(
                    &crate::settings::AISettings::as_ref(ctx).conversation_speech_engine,
                );
                voice::local_model::SpeechOutputModel::new(engine)
            });
        }
        if !ctx.has_singleton_model::<voice::setup::VoiceSetup>() {
            ctx.add_singleton_model(voice::setup::VoiceSetup::new);
        }
        if !ctx.has_singleton_model::<voice::setup::VoiceAutoStart>() {
            ctx.add_singleton_model(voice::setup::VoiceAutoStart::new);
        }
        if ctx.has_singleton_model::<crate::ai::blocklist::BlocklistAIHistoryModel>()
            && !ctx.has_singleton_model::<voice::conversation::ConversationSession>()
        {
            ctx.add_singleton_model(voice::conversation::ConversationSession::new);
        }
    });
}
