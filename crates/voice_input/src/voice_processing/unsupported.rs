//! Stand-ins for platforms without a voice-processing unit; acquiring always fails, so callers
//! fall back to cpal.

use super::{CaptureSink, OnFinished};

pub(crate) enum CaptureLease {}

pub(crate) enum OutputLease {}

impl OutputLease {
    pub(crate) fn output_rate(&self) -> u32 {
        match *self {}
    }

    pub(crate) fn is_running(&self) -> bool {
        match *self {}
    }

    pub(crate) fn play(&self, _samples: &[f32], _on_finished: OnFinished) {
        match *self {}
    }

    pub(crate) fn stop(&self) {
        match *self {}
    }
}

pub(crate) fn acquire_capture(_sink: CaptureSink) -> anyhow::Result<CaptureLease> {
    anyhow::bail!("echo-cancelling audio isn't supported in this build")
}

pub(crate) fn acquire_output() -> anyhow::Result<OutputLease> {
    anyhow::bail!("echo-cancelling audio isn't supported in this build")
}
