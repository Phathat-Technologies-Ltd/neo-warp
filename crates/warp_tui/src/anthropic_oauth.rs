use ai::anthropic_subscription::oauth::{
    AnthropicOAuthFlow, ManualCodeExchange, PkceParams, TokenResponse, authorize_url_for_flow,
};
use ai::api_keys::ApiKeyManager;
use warpui::SingletonEntity as _;
use warpui_core::{Entity, ModelContext};

const EMPTY_CODE_MESSAGE: &str = "Enter the code shown in your browser to finish connecting.";
const MANUAL_FAILURE_MESSAGE: &str =
    "Couldn't connect Claude with that code. Check the code and try again.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TuiAnthropicOAuthControllerEvent {
    Connected,
    Updated,
}

#[derive(Debug)]
enum TuiAnthropicOAuthPhase {
    Waiting { manual_error: Option<String> },
    ExchangingManualCode,
}

pub(crate) struct TuiAnthropicOAuthController {
    manual_exchange: Option<ManualCodeExchange>,
    phase: TuiAnthropicOAuthPhase,
    active: bool,
}

impl TuiAnthropicOAuthController {
    pub(crate) fn new(flow: AnthropicOAuthFlow, ctx: &mut ModelContext<Self>) -> Self {
        let pkce = PkceParams::generate();
        ctx.open_url(&authorize_url_for_flow(&pkce, flow));
        Self {
            manual_exchange: Some(ManualCodeExchange {
                verifier: pkce.verifier,
            }),
            phase: TuiAnthropicOAuthPhase::Waiting { manual_error: None },
            active: true,
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn error(&self) -> Option<&str> {
        match &self.phase {
            TuiAnthropicOAuthPhase::Waiting { manual_error } => manual_error.as_deref(),
            TuiAnthropicOAuthPhase::ExchangingManualCode => None,
        }
    }

    pub(crate) fn submit_manual_code(&mut self, code: String, ctx: &mut ModelContext<Self>) {
        if !self.active || !matches!(self.phase, TuiAnthropicOAuthPhase::Waiting { .. }) {
            return;
        }
        let Some(exchange) = self.manual_exchange.clone() else {
            return;
        };
        if code.trim().is_empty() {
            self.phase = TuiAnthropicOAuthPhase::Waiting {
                manual_error: Some(EMPTY_CODE_MESSAGE.to_owned()),
            };
            ctx.emit(TuiAnthropicOAuthControllerEvent::Updated);
            return;
        }
        self.phase = TuiAnthropicOAuthPhase::ExchangingManualCode;
        ctx.emit(TuiAnthropicOAuthControllerEvent::Updated);
        ctx.spawn(
            async move { exchange.exchange(&code).await },
            |controller, result, ctx| controller.handle_manual_result(result, ctx),
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
        result: anyhow::Result<TokenResponse>,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.active {
            return;
        }
        match result {
            Ok(tokens) => {
                self.active = false;
                self.manual_exchange = None;
                ApiKeyManager::handle(ctx).update(ctx, move |manager, ctx| {
                    manager.store_anthropic_tokens(tokens, ctx);
                });
                ctx.emit(TuiAnthropicOAuthControllerEvent::Connected);
            }
            Err(_) => {
                self.phase = TuiAnthropicOAuthPhase::Waiting {
                    manual_error: Some(MANUAL_FAILURE_MESSAGE.to_owned()),
                };
                ctx.emit(TuiAnthropicOAuthControllerEvent::Updated);
            }
        }
    }
}

impl Entity for TuiAnthropicOAuthController {
    type Event = TuiAnthropicOAuthControllerEvent;
}

#[cfg(test)]
#[path = "anthropic_oauth_tests.rs"]
mod tests;
