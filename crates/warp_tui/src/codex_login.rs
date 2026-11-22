//! View-independent ChatGPT plan login (a device code the user approves in the browser), used by
//! the `/api-keys` inline menu.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ai::api_keys::ApiKeyManager;
use ai::codex_login::oauth::{self, DeviceCode, TokenResponse};
use warpui::SingletonEntity as _;
use warpui_core::{Entity, ModelContext};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TuiCodexLoginControllerEvent {
    Connected,
    Updated,
}

#[derive(Debug)]
enum Phase {
    RequestingCode,
    WaitingForApproval { user_code: String },
    Failed(String),
}

pub(crate) struct TuiCodexLoginController {
    phase: Phase,
    cancelled: Arc<AtomicBool>,
}

impl TuiCodexLoginController {
    pub(crate) fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.spawn(oauth::request_device_code(), |controller, result, ctx| {
            controller.handle_device_code(result, ctx);
        });
        Self {
            phase: Phase::RequestingCode,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        !self.cancelled.load(Ordering::Relaxed) && !matches!(self.phase, Phase::Failed(_))
    }

    /// What the user should do next, or why the login failed.
    pub(crate) fn prompt(&self) -> String {
        match &self.phase {
            Phase::RequestingCode => "Getting a ChatGPT sign-in code...".to_owned(),
            Phase::WaitingForApproval { user_code } => format!(
                "Open {} and enter {user_code}",
                oauth::VERIFICATION_URL.trim_start_matches("https://")
            ),
            Phase::Failed(error) => error.clone(),
        }
    }

    pub(crate) fn cancel(&mut self, ctx: &mut ModelContext<Self>) {
        if !self.cancelled.swap(true, Ordering::Relaxed) {
            ctx.emit(TuiCodexLoginControllerEvent::Updated);
        }
    }

    fn handle_device_code(
        &mut self,
        result: anyhow::Result<DeviceCode>,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.cancelled.load(Ordering::Relaxed) {
            return;
        }
        let code = match result {
            Ok(code) => code,
            Err(error) => {
                log::warn!("Couldn't start the ChatGPT login: {error:#}");
                self.fail(
                    "Couldn't reach OpenAI to sign in. Press Esc and try again.",
                    ctx,
                );
                return;
            }
        };
        self.phase = Phase::WaitingForApproval {
            user_code: code.user_code.clone(),
        };
        ctx.open_url(oauth::VERIFICATION_URL);
        ctx.spawn(
            oauth::wait_for_approval(code, self.cancelled.clone()),
            |controller, result, ctx| controller.handle_approval(result, ctx),
        );
        ctx.emit(TuiCodexLoginControllerEvent::Updated);
    }

    fn handle_approval(
        &mut self,
        result: anyhow::Result<TokenResponse>,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.cancelled.load(Ordering::Relaxed) {
            return;
        }
        match result {
            Ok(tokens) => {
                self.cancelled.store(true, Ordering::Relaxed);
                ApiKeyManager::handle(ctx).update(ctx, move |manager, ctx| {
                    manager.store_codex_tokens(tokens, ctx);
                });
                ctx.emit(TuiCodexLoginControllerEvent::Connected);
            }
            Err(error) => {
                log::warn!("The ChatGPT login failed: {error:#}");
                self.fail(
                    "The ChatGPT sign-in didn't complete. Press Esc and try again.",
                    ctx,
                );
            }
        }
    }

    fn fail(&mut self, message: &str, ctx: &mut ModelContext<Self>) {
        self.phase = Phase::Failed(message.to_owned());
        ctx.emit(TuiCodexLoginControllerEvent::Updated);
    }
}

impl Drop for TuiCodexLoginController {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl Entity for TuiCodexLoginController {
    type Event = TuiCodexLoginControllerEvent;
}
