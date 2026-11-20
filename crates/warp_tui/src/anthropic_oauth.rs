use ai::anthropic_subscription::oauth::{
    AnthropicOAuthFlow, ManualCodeExchange, PkceParams, authorize_url_for_flow,
};
use ai::api_keys::ApiKeyManager;
use warpui::SingletonEntity as _;
use warpui_core::{Entity, ModelContext};

const MANUAL_FAILURE_MESSAGE: &str =
    "Couldn't connect Claude with that code. Check the code and try again.";
const CALLBACK_FAILURE_MESSAGE: &str =
    "Couldn't complete Claude authorization. Press Esc, then select Claude to try again.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TuiAnthropicOAuthControllerEvent {
    Connected,
    Updated,
}

#[derive(Debug)]
enum TuiAnthropicOAuthPhase {
    Waiting { manual_error: Option<String> },
    ExchangingManualCode,
    Fatal(String),
}

pub(crate) struct TuiAnthropicOAuthController {
    flow: AnthropicOAuthFlow,
    pkce: PkceParams,
    manual_exchange: Option<ManualCodeExchange>,
    phase: TuiAnthropicOAuthPhase,
    active: bool,
}

impl TuiAnthropicOAuthController {
    pub(crate) fn new(flow: AnthropicOAuthFlow, ctx: &mut ModelContext<Self>) -> Self {
        let pkce = PkceParams::generate();
        let url = authorize_url_for_flow(&pkce, flow);
        ctx.open_url(&url);
        let manual_exchange = ManualCodeExchange {
            verifier: pkce.verifier.clone(),
        };
        let mut ctrl = Self {
            flow,
            pkce,
            manual_exchange: Some(manual_exchange),
            phase: TuiAnthropicOAuthPhase::Waiting { manual_error: None },
            active: true,
        };
        // Headless / env fallback: if code already in env, submit immediately.
        if let Some(code) = anthropic_env_code() {
            let code = code.clone();
            // Defer to next tick so caller can subscribe before event fires.
            // Use a no-op spawn to enqueue; then submit on callback.
            // Simplest: directly handle via ctx.spawn with exchange future.
            ctrl.submit_manual_code(code, ctx);
        }
        ctrl
    }

    pub(crate) fn flow(&self) -> AnthropicOAuthFlow {
        self.flow
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn is_exchanging(&self) -> bool {
        matches!(self.phase, TuiAnthropicOAuthPhase::ExchangingManualCode)
    }

    pub(crate) fn error(&self) -> Option<&str> {
        match &self.phase {
            TuiAnthropicOAuthPhase::Waiting { manual_error } => manual_error.as_deref(),
            TuiAnthropicOAuthPhase::ExchangingManualCode => None,
            TuiAnthropicOAuthPhase::Fatal(e) => Some(e),
        }
    }

    pub(crate) fn submit_manual_code(&mut self, code: String, ctx: &mut ModelContext<Self>) {
        if !self.active {
            return;
        }
        if !matches!(self.phase, TuiAnthropicOAuthPhase::Waiting { .. }) {
            return;
        }
        let Some(exchange) = self.manual_exchange.clone() else {
            return;
        };
        if code.trim().is_empty() {
            self.phase = TuiAnthropicOAuthPhase::Waiting {
                manual_error: Some("Enter the code shown in your browser to finish connecting.".to_owned()),
            };
            ctx.emit(TuiAnthropicOAuthControllerEvent::Updated);
            return;
        }
        self.phase = TuiAnthropicOAuthPhase::ExchangingManualCode;
        ctx.emit(TuiAnthropicOAuthControllerEvent::Updated);
        ctx.spawn(
            async move { exchange.exchange(&code).await },
            move |controller, result, ctx| {
                controller.handle_manual_result(result, ctx);
            },
        );
    }

    pub(crate) fn clear_manual_error(&mut self, ctx: &mut ModelContext<Self>) {
        if let TuiAnthropicOAuthPhase::Waiting { manual_error } = &mut self.phase
            && manual_error.take().is_some()
        {
            ctx.emit(TuiAnthropicOAuthControllerEvent::Updated);
        }
    }

    pub(crate) fn cancel(&mut self, ctx: &mut ModelContext<Self>) {
        if !self.active {
            return;
        }
        self.active = false;
        self.manual_exchange = None;
        ctx.emit(TuiAnthropicOAuthControllerEvent::Updated);
    }

    fn handle_manual_result(
        &mut self,
        result: anyhow::Result<ai::anthropic_subscription::oauth::TokenResponse>,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.active {
            return;
        }
        match result {
            Ok(tokens) => self.finish_success(tokens, ctx),
            Err(_) => {
                self.phase = TuiAnthropicOAuthPhase::Waiting {
                    manual_error: Some(MANUAL_FAILURE_MESSAGE.to_owned()),
                };
                ctx.emit(TuiAnthropicOAuthControllerEvent::Updated);
            }
        }
    }

    fn finish_success(
        &mut self,
        tokens: ai::anthropic_subscription::oauth::TokenResponse,
        ctx: &mut ModelContext<Self>,
    ) {
        self.active = false;
        self.manual_exchange = None;
        ApiKeyManager::handle(ctx).update(ctx, move |manager, ctx| {
            manager.store_anthropic_tokens(tokens, ctx);
        });
        ctx.emit(TuiAnthropicOAuthControllerEvent::Connected);
    }
}

fn anthropic_env_code() -> Option<String> {
    std::env::var("ANTHROPIC_OAUTH_CODE")
        .or_else(|_| std::env::var("CODE"))
        .ok()
        .filter(|v| !v.trim().is_empty())
}

impl Entity for TuiAnthropicOAuthController {
    type Event = TuiAnthropicOAuthControllerEvent;
}

#[cfg(test)]
#[path = "anthropic_oauth_tests.rs"]
mod tests;