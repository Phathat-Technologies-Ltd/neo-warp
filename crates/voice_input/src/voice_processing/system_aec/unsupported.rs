//! Stand-ins for platforms without process taps; the system pipeline is never available.

use crate::voice_processing::CaptureSink;

pub(crate) enum SystemCaptureLease {}

pub(super) fn os_supported() -> bool {
    false
}

pub(super) fn is_running() -> bool {
    false
}

pub(super) fn is_cancelling() -> bool {
    false
}

pub(super) fn shutdown() {}

pub(super) fn on_noise_suppression_changed() {}

pub(super) fn acquire_capture(_sink: CaptureSink) -> anyhow::Result<SystemCaptureLease> {
    anyhow::bail!("system echo cancellation isn't supported in this build")
}
