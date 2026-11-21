use ai::anthropic_subscription::ant_cli;
use ai::anthropic_subscription::oauth::{
    AnthropicOAuthFlow, ManualCodeExchange, PkceParams, TokenResponse, authorize_url_for_flow,
};
use ai::api_keys::ApiKeyManager;
use warpui::SingletonEntity as _;
use warpui_core::{Entity, ModelContext};

const EMPTY_CODE_MESSAGE: &str = "Enter the code shown in your browser to finish connecting.";
const MANUAL_FAILURE_MESSAGE: &str =
    "Couldn't connect Claude with that code. Check the code and try again.";
const PASTE_CODE_PROMPT: &str = "Connect Claude — paste CODE#STATE";
const BROWSER_SIGN_IN_PROMPT: &str = "Connect Claude — finish signing in in your browser";
const ANT_FAILURE_MESSAGE: &str =
    "Couldn't sign in to Claude. Press Esc, then select Claude to try again.";

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
    /// Whether the sign-in runs through Anthropic's `ant` CLI rather than a pasted code.
    uses_ant_cli: bool,
    manual_exchange: Option<ManualCodeExchange>,
    phase: TuiAnthropicOAuthPhase,
    active: bool,
}

impl TuiAnthropicOAuthController {
    pub(crate) fn new(flow: AnthropicOAuthFlow, ctx: &mut ModelContext<Self>) -> Self {
        // Console sign-in goes through Anthropic's own CLI when it's available.
        if flow == AnthropicOAuthFlow::Console
            && let Some(ant) = ant_cli::get()
        {
            ctx.spawn(
                async move {
                    ant.login().await?;
                    ant.token().await
                },
                |controller, result, ctx| controller.handle_ant_cli_result(result, ctx),
            );
            return Self {
                uses_ant_cli: true,
                manual_exchange: None,
                phase: TuiAnthropicOAuthPhase::Waiting { manual_error: None },
                active: true,
            };
        }
        let pkce = PkceParams::generate();
        ctx.open_url(&authorize_url_for_flow(&pkce, flow));
        Self {
            uses_ant_cli: false,
            manual_exchange: Some(ManualCodeExchange {
                verifier: pkce.verifier,
            }),
            phase: TuiAnthropicOAuthPhase::Waiting { manual_error: None },
            active: true,
        }
    }

    /// The instruction shown while waiting for the user.
    pub(crate) fn prompt(&self) -> &'static str {
        if self.uses_ant_cli {
            BROWSER_SIGN_IN_PROMPT
        } else {
            PASTE_CODE_PROMPT
        }
    }

    fn handle_ant_cli_result(
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
                ApiKeyManager::handle(ctx).update(ctx, move |manager, ctx| {
                    manager.store_ant_cli_tokens(tokens, ctx);
                });
                ctx.emit(TuiAnthropicOAuthControllerEvent::Connected);
            }
            Err(error) => {
                log::warn!("Claude sign-in through the ant CLI failed: {error:#}");
                self.phase = TuiAnthropicOAuthPhase::Waiting {
                    manual_error: Some(ANT_FAILURE_MESSAGE.to_owned()),
                };
                ctx.emit(TuiAnthropicOAuthControllerEvent::Updated);
            }
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
