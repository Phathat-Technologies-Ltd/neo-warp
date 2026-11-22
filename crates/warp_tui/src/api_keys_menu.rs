//! Stateful `/api-keys` inline menu backed by the shared TUI input editor.

use ai::LLMProvider;
use ai::anthropic_subscription::oauth::AnthropicOAuthFlow;
use ai::api_keys::{ApiKeyManager, ApiKeyManagerEvent, CustomEndpointId};
use ai::grok_subscription::oauth::OauthAttempt;
use warp::TuiLoginModel;
use warp::editor::{CodeEditorModel, CodeEditorModelEvent};
use warp::settings::{AISettings, AISettingsChangedEvent};
use warp::tui_export::{TeamContextResolver, UserWorkspaces};
use warp_core::features::FeatureFlag;
use warp_core::settings::ToggleableSetting as _;
use warp_editor::model::CoreEditorModel;
use warpui::SingletonEntity as _;
use warpui_core::elements::tui::{TuiElement, TuiText};
use warpui_core::{AppContext, Entity, ModelContext, ModelHandle};

use crate::anthropic_oauth::{TuiAnthropicOAuthController, TuiAnthropicOAuthControllerEvent};
use crate::codex_login::{TuiCodexLoginController, TuiCodexLoginControllerEvent};
use crate::grok_oauth::{TuiGrokOAuthController, TuiGrokOAuthControllerEvent};
use crate::inline_menu::{
    MAX_INLINE_MENU_ROWS, TuiInlineMenuHeader, TuiInlineMenuInputOwnership, TuiInlineMenuListState,
    TuiInlineMenuRow, TuiInlineMenuRowStyle, TuiInlineMenuScrollAnchor, TuiInlineMenuSnapshot,
    TuiInlineMenuStatus, result_row_capacity,
};
use crate::input_suggestions_mode::{
    TuiInputSuggestionsMode, TuiInputSuggestionsModeEvent, TuiInputSuggestionsModeModel,
};
use crate::tui_builder::TuiUiBuilder;

const MAX_VISIBLE_ROWS: usize = result_row_capacity(MAX_INLINE_MENU_ROWS, true, false);
const FALLBACK_DESCRIPTION: &str = "in the event of an error, requests may be routed to use Warp \
credits. Warp will prioritize using your API keys over Warp credits.";
#[derive(Debug, Clone, PartialEq, Eq)]
enum TuiApiKeysRowKind {
    Provider(LLMProvider),
    ChatGptLogin,
    CustomEndpoint(CustomEndpointId),
    CustomEndpointConfigurationError,
    WarpCreditFallbackSetting,
}
#[derive(Debug, Clone)]
struct TuiApiKeysRow {
    kind: TuiApiKeysRowKind,
    title: String,
    is_selectable: bool,
}

#[derive(Default)]
enum TuiApiKeysMenuState {
    #[default]
    Closed,
    Browsing {
        list: TuiInlineMenuListState<TuiApiKeysRow>,
        error: Option<String>,
    },
    EditingProvider {
        provider: LLMProvider,
        error: Option<String>,
    },
    EditingCustomEndpoint {
        id: CustomEndpointId,
        name: String,
        error: Option<String>,
    },
    ConnectingGrok {
        controller: ModelHandle<TuiGrokOAuthController>,
    },
    AnthropicSelector {
        list: TuiInlineMenuListState<AnthropicSelectorRow>,
    },
    ConnectingAnthropic {
        controller: ModelHandle<TuiAnthropicOAuthController>,
    },
    ConnectingChatGpt {
        controller: ModelHandle<TuiCodexLoginController>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum AnthropicSelectorRowKind {
    PastedKey,
    Console,
    ClaudeCode,
}
#[derive(Debug, Clone)]
struct AnthropicSelectorRow {
    kind: AnthropicSelectorRowKind,
    title: String,
    description: Option<String>,
}
fn anthropic_selector_rows() -> Vec<AnthropicSelectorRow> {
    vec![
        AnthropicSelectorRow {
            kind: AnthropicSelectorRowKind::PastedKey,
            title: "Pasted API Key".to_owned(),
            description: Some("Use a sk-ant-* key".to_owned()),
        },
        AnthropicSelectorRow {
            kind: AnthropicSelectorRowKind::Console,
            title: "Connect with Claude — Console / API".to_owned(),
            description: Some("console.anthropic.com (default)".to_owned()),
        },
        AnthropicSelectorRow {
            kind: AnthropicSelectorRowKind::ClaudeCode,
            title: "Connect with Claude — Claude Code".to_owned(),
            description: Some("claude.ai (Pro/Max/Team)".to_owned()),
        },
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TuiApiKeysFooter {
    ProviderList { can_clear: bool },
    WarpCreditFallback,
    EditingProvider(LLMProvider),
    EditingCustomEndpoint(String),
    ConnectingGrok,
    AnthropicSelector,
    ConnectingAnthropic,
    ConnectingChatGpt,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TuiApiKeysMenuEvent;

pub(crate) struct TuiApiKeysMenuModel {
    input_editor: ModelHandle<CodeEditorModel>,
    suggestions_mode: ModelHandle<TuiInputSuggestionsModeModel>,
    team_context: TeamContextResolver,
    state: TuiApiKeysMenuState,
}

impl TuiApiKeysMenuModel {
    pub(crate) fn new(
        input_editor: ModelHandle<CodeEditorModel>,
        suggestions_mode: ModelHandle<TuiInputSuggestionsModeModel>,
        team_context: TeamContextResolver,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        ctx.subscribe_to_model(&input_editor, |model, _, event, ctx| {
            if !matches!(event, CodeEditorModelEvent::ContentChanged { .. }) {
                return;
            }
            match &mut model.state {
                TuiApiKeysMenuState::Browsing { error, .. } => {
                    error.take();
                    model.refresh_rows(ctx);
                }
                TuiApiKeysMenuState::EditingProvider { error, .. } => {
                    if error.take().is_some() {
                        ctx.emit(TuiApiKeysMenuEvent);
                    }
                }
                TuiApiKeysMenuState::EditingCustomEndpoint { error, .. } => {
                    if error.take().is_some() {
                        ctx.emit(TuiApiKeysMenuEvent);
                    }
                }
                TuiApiKeysMenuState::ConnectingGrok { controller } => {
                    controller.update(ctx, |controller, ctx| {
                        controller.clear_manual_error(ctx);
                    });
                }
                TuiApiKeysMenuState::AnthropicSelector { .. } => {}
                TuiApiKeysMenuState::ConnectingAnthropic { controller } => {
                    controller.update(ctx, |controller, ctx| {
                        controller.clear_manual_error(ctx);
                    });
                }
                TuiApiKeysMenuState::ConnectingChatGpt { .. } | TuiApiKeysMenuState::Closed => {}
            }
        });
        ctx.subscribe_to_model(
            &ApiKeyManager::handle(ctx),
            |model, _, _: &ApiKeyManagerEvent, ctx| {
                if model.is_open(ctx) {
                    model.refresh_rows(ctx);
                }
            },
        );
        ctx.subscribe_to_model(&AISettings::handle(ctx), |model, _, event, ctx| {
            if model.is_open(ctx)
                && matches!(
                    event,
                    AISettingsChangedEvent::CanUseWarpCreditsForFallback { .. }
                )
            {
                model.refresh_rows(ctx);
            }
        });
        ctx.subscribe_to_model(
            &suggestions_mode,
            |model, _, event: &TuiInputSuggestionsModeEvent, ctx| {
                if event.mode != TuiInputSuggestionsMode::ApiKeys {
                    model.deactivate(ctx);
                }
            },
        );
        Self {
            input_editor,
            suggestions_mode,
            team_context,
            state: TuiApiKeysMenuState::Closed,
        }
    }

    /// Whether this session's team allows members to bring their own provider credentials.
    fn member_byo_keys_allowed(&self, ctx: &AppContext) -> bool {
        let scope = (self.team_context)(ctx);
        UserWorkspaces::as_ref(ctx).are_member_byo_keys_allowed(&scope)
    }

    fn start_anthropic_oauth(&mut self, flow: AnthropicOAuthFlow, ctx: &mut ModelContext<Self>) {
        let policy_error = if !UserWorkspaces::as_ref(ctx).is_byo_api_key_enabled(ctx) {
            Some("Claude subscriptions require BYOK access for this workspace.")
        } else if !self.member_byo_keys_allowed(ctx) {
            Some("Your organization doesn't allow member-provided credentials.")
        } else {
            None
        };
        if let Some(error) = policy_error {
            self.set_browsing_error(error.to_owned(), ctx);
            return;
        }
        self.clear_input(ctx);
        let controller = ctx.add_model(move |ctx| TuiAnthropicOAuthController::new(flow, ctx));
        ctx.subscribe_to_model(&controller, |menu, _, event, ctx| match event {
            TuiAnthropicOAuthControllerEvent::Connected => menu.transition_to_browsing(ctx),
            TuiAnthropicOAuthControllerEvent::Updated => ctx.emit(TuiApiKeysMenuEvent),
        });
        self.state = TuiApiKeysMenuState::ConnectingAnthropic { controller };
        ctx.emit(TuiApiKeysMenuEvent);
    }
    fn start_chatgpt_login(&mut self, ctx: &mut ModelContext<Self>) {
        if ApiKeyManager::as_ref(ctx).has_codex_login() {
            self.set_browsing_error(
                "ChatGPT is already connected. Press Ctrl-X to disconnect.".to_owned(),
                ctx,
            );
            return;
        }
        let policy_error = if !UserWorkspaces::as_ref(ctx).is_byo_api_key_enabled(ctx) {
            Some("ChatGPT logins require BYOK access for this workspace.")
        } else if !self.member_byo_keys_allowed(ctx) {
            Some("Your organization doesn't allow member-provided credentials.")
        } else {
            None
        };
        if let Some(error) = policy_error {
            self.set_browsing_error(error.to_owned(), ctx);
            return;
        }
        self.clear_input(ctx);
        let controller = ctx.add_model(TuiCodexLoginController::new);
        ctx.subscribe_to_model(&controller, |menu, _, event, ctx| match event {
            TuiCodexLoginControllerEvent::Connected => menu.transition_to_browsing(ctx),
            TuiCodexLoginControllerEvent::Updated => ctx.emit(TuiApiKeysMenuEvent),
        });
        self.state = TuiApiKeysMenuState::ConnectingChatGpt { controller };
        ctx.emit(TuiApiKeysMenuEvent);
    }

    fn start_grok_oauth(&mut self, ctx: &mut ModelContext<Self>) {
        let policy_error = if !FeatureFlag::SuperGrok.is_enabled() {
            Some("Grok subscriptions aren't available in this build.")
        } else if !UserWorkspaces::as_ref(ctx).is_byo_api_key_enabled(ctx) {
            Some("Grok subscriptions require BYOK access for this workspace.")
        } else if !self.member_byo_keys_allowed(ctx) {
            Some("Your organization doesn't allow member-provided credentials.")
        } else {
            None
        };
        if let Some(error) = policy_error {
            self.set_browsing_error(error.to_owned(), ctx);
            return;
        }
        let attempt = match OauthAttempt::start() {
            Ok(attempt) => attempt,
            Err(error) => {
                self.set_browsing_error(error.to_string(), ctx);
                return;
            }
        };
        self.clear_input(ctx);
        let controller = ctx.add_model(move |ctx| TuiGrokOAuthController::new(attempt, ctx));
        ctx.subscribe_to_model(&controller, |menu, _, event, ctx| match event {
            TuiGrokOAuthControllerEvent::Connected => menu.transition_to_browsing(ctx),
            TuiGrokOAuthControllerEvent::Updated => ctx.emit(TuiApiKeysMenuEvent),
        });
        self.state = TuiApiKeysMenuState::ConnectingGrok { controller };
        ctx.emit(TuiApiKeysMenuEvent);
    }

    pub(crate) fn is_open(&self, ctx: &AppContext) -> bool {
        !matches!(self.state, TuiApiKeysMenuState::Closed)
            && self.suggestions_mode.as_ref(ctx).mode() == TuiInputSuggestionsMode::ApiKeys
    }

    pub(crate) fn open(&mut self, ctx: &mut ModelContext<Self>) {
        if self.is_open(ctx) {
            return;
        }
        let did_open = self.suggestions_mode.update(ctx, |mode, ctx| {
            mode.try_open(TuiInputSuggestionsMode::ApiKeys, ctx)
        });
        if !did_open {
            return;
        }
        self.transition_to_browsing(ctx);
    }

    /// Opens the menu and jumps straight into the Grok connect path, equivalent to selecting the
    /// "X premium or SuperGrok subscription" row. Reuses `edit_provider` so the already-connected
    /// and policy-gated cases surface the same messaging as the provider list.
    pub(crate) fn open_and_connect_grok(&mut self, ctx: &mut ModelContext<Self>) {
        self.open(ctx);
        if self.is_open(ctx) {
            self.edit_provider(LLMProvider::Xai, ctx);
        }
    }

    pub(crate) fn dismiss(&mut self, ctx: &mut ModelContext<Self>) {
        match self.state {
            TuiApiKeysMenuState::Closed => {}
            TuiApiKeysMenuState::Browsing { .. } => self.close(ctx),
            TuiApiKeysMenuState::EditingProvider { .. }
            | TuiApiKeysMenuState::EditingCustomEndpoint { .. }
            | TuiApiKeysMenuState::ConnectingGrok { .. }
            | TuiApiKeysMenuState::AnthropicSelector { .. }
            | TuiApiKeysMenuState::ConnectingAnthropic { .. }
            | TuiApiKeysMenuState::ConnectingChatGpt { .. } => self.transition_to_browsing(ctx),
        }
    }

    /// Returns the shared editor owner for the active API keys state.
    pub(crate) fn input_ownership(&self, ctx: &AppContext) -> TuiInlineMenuInputOwnership {
        if !self.is_open(ctx) {
            return TuiInlineMenuInputOwnership::Composer;
        }
        match self.state {
            TuiApiKeysMenuState::EditingProvider { .. }
            | TuiApiKeysMenuState::EditingCustomEndpoint { .. } => {
                TuiInlineMenuInputOwnership::InlineMenuMasked
            }
            TuiApiKeysMenuState::Browsing { .. }
            | TuiApiKeysMenuState::ConnectingGrok { .. }
            | TuiApiKeysMenuState::AnthropicSelector { .. }
            | TuiApiKeysMenuState::ConnectingAnthropic { .. }
            | TuiApiKeysMenuState::ConnectingChatGpt { .. } => {
                TuiInlineMenuInputOwnership::InlineMenuPlainText
            }
            TuiApiKeysMenuState::Closed => TuiInlineMenuInputOwnership::Composer,
        }
    }

    pub(crate) fn uses_credential_border(&self, ctx: &AppContext) -> bool {
        self.is_open(ctx)
            && matches!(
                self.state,
                TuiApiKeysMenuState::EditingProvider { .. }
                    | TuiApiKeysMenuState::EditingCustomEndpoint { .. }
                    | TuiApiKeysMenuState::ConnectingGrok { .. }
                    | TuiApiKeysMenuState::ConnectingAnthropic { .. }
                    | TuiApiKeysMenuState::ConnectingChatGpt { .. }
            )
    }

    pub(crate) fn footer(&self, ctx: &AppContext) -> Option<TuiApiKeysFooter> {
        if !self.is_open(ctx) {
            return None;
        }
        match &self.state {
            TuiApiKeysMenuState::Closed => None,
            TuiApiKeysMenuState::Browsing { list, .. } => {
                Some(match list.selected_row().map(|row| row.kind.clone()) {
                    Some(TuiApiKeysRowKind::WarpCreditFallbackSetting) => {
                        TuiApiKeysFooter::WarpCreditFallback
                    }
                    Some(TuiApiKeysRowKind::Provider(provider)) => TuiApiKeysFooter::ProviderList {
                        can_clear: provider_connected(provider, ctx),
                    },
                    Some(TuiApiKeysRowKind::ChatGptLogin) => TuiApiKeysFooter::ProviderList {
                        can_clear: ApiKeyManager::as_ref(ctx).has_codex_login(),
                    },
                    Some(TuiApiKeysRowKind::CustomEndpoint(id)) => TuiApiKeysFooter::ProviderList {
                        can_clear: custom_endpoint_connected(&id, ctx),
                    },
                    Some(TuiApiKeysRowKind::CustomEndpointConfigurationError) => {
                        TuiApiKeysFooter::ProviderList { can_clear: false }
                    }
                    None => TuiApiKeysFooter::ProviderList { can_clear: false },
                })
            }
            TuiApiKeysMenuState::EditingProvider { provider, .. } => {
                Some(TuiApiKeysFooter::EditingProvider(*provider))
            }
            TuiApiKeysMenuState::EditingCustomEndpoint { name, .. } => {
                Some(TuiApiKeysFooter::EditingCustomEndpoint(name.clone()))
            }
            TuiApiKeysMenuState::ConnectingGrok { .. } => Some(TuiApiKeysFooter::ConnectingGrok),
            TuiApiKeysMenuState::AnthropicSelector { .. } => {
                Some(TuiApiKeysFooter::AnthropicSelector)
            }
            TuiApiKeysMenuState::ConnectingAnthropic { .. } => {
                Some(TuiApiKeysFooter::ConnectingAnthropic)
            }
            TuiApiKeysMenuState::ConnectingChatGpt { .. } => {
                Some(TuiApiKeysFooter::ConnectingChatGpt)
            }
        }
    }

    pub(crate) fn can_clear_selected(&self, ctx: &AppContext) -> bool {
        match &self.state {
            TuiApiKeysMenuState::Browsing { list, .. } => {
                match list.selected_row().map(|row| row.kind.clone()) {
                    Some(TuiApiKeysRowKind::Provider(provider)) => {
                        provider_connected(provider, ctx)
                    }
                    Some(TuiApiKeysRowKind::ChatGptLogin) => {
                        ApiKeyManager::as_ref(ctx).has_codex_login()
                    }
                    Some(TuiApiKeysRowKind::CustomEndpoint(id)) => {
                        custom_endpoint_connected(&id, ctx)
                    }
                    Some(TuiApiKeysRowKind::CustomEndpointConfigurationError) => false,
                    Some(TuiApiKeysRowKind::WarpCreditFallbackSetting) | None => false,
                }
            }
            TuiApiKeysMenuState::Closed
            | TuiApiKeysMenuState::EditingProvider { .. }
            | TuiApiKeysMenuState::EditingCustomEndpoint { .. }
            | TuiApiKeysMenuState::ConnectingGrok { .. }
            | TuiApiKeysMenuState::AnthropicSelector { .. }
            | TuiApiKeysMenuState::ConnectingAnthropic { .. }
            | TuiApiKeysMenuState::ConnectingChatGpt { .. } => false,
        }
    }

    pub(crate) fn clear_selected(&mut self, ctx: &mut ModelContext<Self>) {
        let kind = match &self.state {
            TuiApiKeysMenuState::Browsing { list, .. } => {
                match list.selected_row().map(|row| row.kind.clone()) {
                    Some(kind @ TuiApiKeysRowKind::Provider(_))
                    | Some(kind @ TuiApiKeysRowKind::ChatGptLogin)
                    | Some(kind @ TuiApiKeysRowKind::CustomEndpoint(_)) => kind,
                    Some(
                        TuiApiKeysRowKind::CustomEndpointConfigurationError
                        | TuiApiKeysRowKind::WarpCreditFallbackSetting,
                    )
                    | None => return,
                }
            }
            TuiApiKeysMenuState::Closed
            | TuiApiKeysMenuState::EditingProvider { .. }
            | TuiApiKeysMenuState::EditingCustomEndpoint { .. }
            | TuiApiKeysMenuState::ConnectingGrok { .. }
            | TuiApiKeysMenuState::AnthropicSelector { .. }
            | TuiApiKeysMenuState::ConnectingAnthropic { .. }
            | TuiApiKeysMenuState::ConnectingChatGpt { .. } => return,
        };
        let result = match kind {
            TuiApiKeysRowKind::ChatGptLogin => {
                ApiKeyManager::handle(ctx).update(ctx, |manager, ctx| {
                    manager.disconnect_codex_login(ctx);
                });
                Ok(())
            }
            TuiApiKeysRowKind::Provider(LLMProvider::Xai) => {
                ApiKeyManager::handle(ctx).update(ctx, |manager, ctx| {
                    manager.set_grok_tokens(None, ctx);
                });
                Ok(())
            }
            TuiApiKeysRowKind::Provider(LLMProvider::Anthropic)
                if ApiKeyManager::as_ref(ctx).has_anthropic_subscription() =>
            {
                ApiKeyManager::handle(ctx).update(ctx, |manager, ctx| {
                    manager.disconnect_anthropic_subscription(ctx);
                });
                Ok(())
            }
            TuiApiKeysRowKind::Provider(provider) => ApiKeyManager::handle(ctx)
                .update(ctx, |manager, ctx| {
                    manager.persist_provider_key(provider, None, ctx)
                }),
            TuiApiKeysRowKind::CustomEndpoint(id) => ApiKeyManager::handle(ctx)
                .update(ctx, |manager, ctx| {
                    manager.persist_custom_endpoint_key(id, None, ctx)
                }),
            TuiApiKeysRowKind::CustomEndpointConfigurationError
            | TuiApiKeysRowKind::WarpCreditFallbackSetting => return,
        };
        match result {
            Ok(()) => self.refresh_rows(ctx),
            Err(_) => self.set_browsing_error(
                "Could not clear the selected API key. Try again.".to_owned(),
                ctx,
            ),
        }
    }

    pub(crate) fn select_previous(&mut self, ctx: &mut ModelContext<Self>) {
        match &mut self.state {
            TuiApiKeysMenuState::Browsing { list, .. } => {
                list.select_previous(MAX_VISIBLE_ROWS, |row| row.is_selectable);
                ctx.emit(TuiApiKeysMenuEvent);
            }
            TuiApiKeysMenuState::AnthropicSelector { list } => {
                list.select_previous(MAX_VISIBLE_ROWS, |_| true);
                ctx.emit(TuiApiKeysMenuEvent);
            }
            TuiApiKeysMenuState::Closed
            | TuiApiKeysMenuState::EditingProvider { .. }
            | TuiApiKeysMenuState::EditingCustomEndpoint { .. }
            | TuiApiKeysMenuState::ConnectingGrok { .. }
            | TuiApiKeysMenuState::ConnectingAnthropic { .. }
            | TuiApiKeysMenuState::ConnectingChatGpt { .. } => {}
        }
    }

    pub(crate) fn select_next(&mut self, ctx: &mut ModelContext<Self>) {
        match &mut self.state {
            TuiApiKeysMenuState::Browsing { list, .. } => {
                list.select_next(MAX_VISIBLE_ROWS, |row| row.is_selectable);
                ctx.emit(TuiApiKeysMenuEvent);
            }
            TuiApiKeysMenuState::AnthropicSelector { list } => {
                list.select_next(MAX_VISIBLE_ROWS, |_| true);
                ctx.emit(TuiApiKeysMenuEvent);
            }
            TuiApiKeysMenuState::Closed
            | TuiApiKeysMenuState::EditingProvider { .. }
            | TuiApiKeysMenuState::EditingCustomEndpoint { .. }
            | TuiApiKeysMenuState::ConnectingGrok { .. }
            | TuiApiKeysMenuState::ConnectingAnthropic { .. }
            | TuiApiKeysMenuState::ConnectingChatGpt { .. } => {}
        }
    }

    pub(crate) fn select_at_snapshot_index(
        &mut self,
        index: usize,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let TuiApiKeysMenuState::Browsing { list, .. } = &mut self.state else {
            return false;
        };
        let selected = list.select_absolute(index, MAX_VISIBLE_ROWS, |row| row.is_selectable);
        ctx.emit(TuiApiKeysMenuEvent);
        selected
    }

    pub(crate) fn scroll_by_delta(&mut self, delta: isize, ctx: &mut ModelContext<Self>) {
        let TuiApiKeysMenuState::Browsing { list, .. } = &mut self.state else {
            return;
        };
        list.scroll_by(delta, MAX_VISIBLE_ROWS);
        ctx.emit(TuiApiKeysMenuEvent);
    }

    pub(crate) fn accept_selected(&mut self, ctx: &mut ModelContext<Self>) {
        match &self.state {
            TuiApiKeysMenuState::Closed => {}
            TuiApiKeysMenuState::Browsing { list, .. } => {
                let Some(kind) = list.selected_row().map(|row| row.kind.clone()) else {
                    return;
                };
                match kind {
                    TuiApiKeysRowKind::Provider(provider) => self.edit_provider(provider, ctx),
                    TuiApiKeysRowKind::ChatGptLogin => self.start_chatgpt_login(ctx),
                    TuiApiKeysRowKind::CustomEndpoint(id) => self.edit_custom_endpoint(id, ctx),
                    TuiApiKeysRowKind::CustomEndpointConfigurationError => {}
                    TuiApiKeysRowKind::WarpCreditFallbackSetting => self.toggle_fallback(ctx),
                }
            }
            TuiApiKeysMenuState::EditingProvider { provider, .. } => {
                let provider = *provider;
                self.save_provider(provider, ctx);
            }
            TuiApiKeysMenuState::EditingCustomEndpoint { id, .. } => {
                self.save_custom_endpoint(id.clone(), ctx);
            }
            TuiApiKeysMenuState::AnthropicSelector { list } => {
                let kind = list.selected_row().map(|r| r.kind.clone());
                match kind {
                    Some(AnthropicSelectorRowKind::PastedKey) => {
                        let key = LLMProvider::Anthropic
                            .api_key(ApiKeyManager::as_ref(ctx).keys())
                            .unwrap_or_default()
                            .to_owned();
                        self.set_input(&key, ctx);
                        self.state = TuiApiKeysMenuState::EditingProvider {
                            provider: LLMProvider::Anthropic,
                            error: None,
                        };
                        ctx.emit(TuiApiKeysMenuEvent);
                    }
                    Some(AnthropicSelectorRowKind::Console) => {
                        self.start_anthropic_oauth(AnthropicOAuthFlow::Console, ctx)
                    }
                    Some(AnthropicSelectorRowKind::ClaudeCode) => {
                        self.start_anthropic_oauth(AnthropicOAuthFlow::ClaudeCode, ctx)
                    }
                    None => {}
                }
            }
            TuiApiKeysMenuState::ConnectingGrok { controller } => {
                let controller = controller.clone();
                let code = input_text(&self.input_editor, ctx);
                if !code.trim().is_empty() {
                    self.clear_input(ctx);
                }
                controller.update(ctx, |controller, ctx| {
                    controller.submit_manual_code(code, ctx);
                });
            }
            TuiApiKeysMenuState::ConnectingAnthropic { controller } => {
                let controller = controller.clone();
                let code = input_text(&self.input_editor, ctx);
                if !code.trim().is_empty() {
                    self.clear_input(ctx);
                }
                controller.update(ctx, |controller, ctx| {
                    controller.submit_manual_code(code, ctx);
                });
            }
            TuiApiKeysMenuState::ConnectingChatGpt { .. } => {}
        }
    }

    pub(crate) fn snapshot(&self, ctx: &AppContext) -> Option<TuiInlineMenuSnapshot> {
        if !self.is_open(ctx) {
            return None;
        }
        match &self.state {
            TuiApiKeysMenuState::Closed => None,
            TuiApiKeysMenuState::Browsing { list, error } => Some(TuiInlineMenuSnapshot {
                header: Some(TuiInlineMenuHeader {
                    title: Some(error.clone().unwrap_or_else(|| "API keys".to_owned())),
                    tabs: Vec::new(),
                }),
                rows: list
                    .rows()
                    .iter()
                    .map(|row| self.snapshot_row(row, ctx, None))
                    .collect(),
                selected_index: list.selected_index(),
                scroll_offset: list.scroll_offset(),
                scroll_anchor: list.scroll_anchor(),
                max_visible_rows: MAX_VISIBLE_ROWS,
                status: None,
            }),
            TuiApiKeysMenuState::EditingProvider { provider, error } => {
                Some(TuiInlineMenuSnapshot {
                    header: Some(TuiInlineMenuHeader {
                        title: Some(format!("{} API key", provider.display_name())),
                        tabs: Vec::new(),
                    }),
                    rows: Vec::new(),
                    selected_index: None,
                    scroll_offset: 0,
                    scroll_anchor: TuiInlineMenuScrollAnchor::Selection,
                    max_visible_rows: MAX_VISIBLE_ROWS,
                    status: error.clone().map(TuiInlineMenuStatus::Empty),
                })
            }
            TuiApiKeysMenuState::EditingCustomEndpoint { name, error, .. } => {
                Some(TuiInlineMenuSnapshot {
                    header: Some(TuiInlineMenuHeader {
                        title: Some(error.clone().unwrap_or_else(|| format!("{name} API key"))),
                        tabs: Vec::new(),
                    }),
                    rows: Vec::new(),
                    selected_index: None,
                    scroll_offset: 0,
                    scroll_anchor: TuiInlineMenuScrollAnchor::Selection,
                    max_visible_rows: MAX_VISIBLE_ROWS,
                    status: error.clone().map(TuiInlineMenuStatus::Empty),
                })
            }
            TuiApiKeysMenuState::AnthropicSelector { list } => Some(TuiInlineMenuSnapshot {
                header: Some(TuiInlineMenuHeader {
                    title: Some("Anthropic — choose method".to_owned()),
                    tabs: Vec::new(),
                }),
                rows: list
                    .rows()
                    .iter()
                    .map(|row| TuiInlineMenuRow {
                        title: row.title.clone(),
                        prefix: None,
                        description: row.description.clone(),
                        state_suffix: None,
                        promotional_suffix: None,
                        is_selectable: true,
                        style: TuiInlineMenuRowStyle::InlineMenuItem,
                    })
                    .collect(),
                selected_index: list.selected_index(),
                scroll_offset: list.scroll_offset(),
                scroll_anchor: list.scroll_anchor(),
                max_visible_rows: MAX_VISIBLE_ROWS,
                status: None,
            }),
            TuiApiKeysMenuState::ConnectingGrok { controller } => {
                let error = controller.as_ref(ctx).error().map(ToOwned::to_owned);
                let rows = all_rows(ctx);
                let connecting = TuiApiKeysRowKind::Provider(LLMProvider::Xai);
                let selected_index = row_index(&rows, &connecting);
                let rows = rows
                    .iter()
                    .map(|row| self.snapshot_row(row, ctx, Some(&connecting)))
                    .collect();
                Some(TuiInlineMenuSnapshot {
                    header: Some(TuiInlineMenuHeader {
                        title: Some(error.unwrap_or_else(|| "API keys".to_owned())),
                        tabs: Vec::new(),
                    }),
                    rows,
                    selected_index,
                    scroll_offset: 0,
                    scroll_anchor: TuiInlineMenuScrollAnchor::Selection,
                    max_visible_rows: MAX_VISIBLE_ROWS,
                    status: None,
                })
            }
            TuiApiKeysMenuState::ConnectingAnthropic { controller } => {
                let error = controller.as_ref(ctx).error().map(ToOwned::to_owned);
                let rows = all_rows(ctx);
                let connecting = TuiApiKeysRowKind::Provider(LLMProvider::Anthropic);
                let selected_index = row_index(&rows, &connecting);
                let rows = rows
                    .iter()
                    .map(|row| self.snapshot_row(row, ctx, Some(&connecting)))
                    .collect();
                Some(TuiInlineMenuSnapshot {
                    header: Some(TuiInlineMenuHeader {
                        title: Some(
                            error.unwrap_or_else(|| controller.as_ref(ctx).prompt().to_owned()),
                        ),
                        tabs: Vec::new(),
                    }),
                    rows,
                    selected_index,
                    scroll_offset: 0,
                    scroll_anchor: TuiInlineMenuScrollAnchor::Selection,
                    max_visible_rows: MAX_VISIBLE_ROWS,
                    status: None,
                })
            }
            TuiApiKeysMenuState::ConnectingChatGpt { controller } => {
                let rows = all_rows(ctx);
                let connecting = TuiApiKeysRowKind::ChatGptLogin;
                let selected_index = row_index(&rows, &connecting);
                let rows = rows
                    .iter()
                    .map(|row| self.snapshot_row(row, ctx, Some(&connecting)))
                    .collect();
                Some(TuiInlineMenuSnapshot {
                    header: Some(TuiInlineMenuHeader {
                        title: Some(controller.as_ref(ctx).prompt()),
                        tabs: Vec::new(),
                    }),
                    rows,
                    selected_index,
                    scroll_offset: 0,
                    scroll_anchor: TuiInlineMenuScrollAnchor::Selection,
                    max_visible_rows: MAX_VISIBLE_ROWS,
                    status: None,
                })
            }
        }
    }

    fn snapshot_row(
        &self,
        row: &TuiApiKeysRow,
        ctx: &AppContext,
        connecting: Option<&TuiApiKeysRowKind>,
    ) -> TuiInlineMenuRow {
        let is_connecting = connecting.is_some();
        let (description, state_suffix, is_selectable) = match &row.kind {
            TuiApiKeysRowKind::ChatGptLogin => {
                let suffix = if connecting == Some(&row.kind) {
                    "(Connecting...)"
                } else if ApiKeyManager::as_ref(ctx).has_codex_login() {
                    "(Connected)"
                } else {
                    "(Not connected)"
                };
                (Some(String::new()), Some(suffix.to_owned()), !is_connecting)
            }
            TuiApiKeysRowKind::Provider(provider) => {
                let connected = provider_connected(*provider, ctx);
                let suffix = if connecting == Some(&row.kind) {
                    "(Connecting...)".to_owned()
                } else if *provider == LLMProvider::Anthropic && connected {
                    format!("({})", anthropic_connected_suffix(ctx))
                } else if connected {
                    "(Connected)".to_owned()
                } else {
                    "(Not connected)".to_owned()
                };
                (Some(String::new()), Some(suffix.to_owned()), !is_connecting)
            }
            TuiApiKeysRowKind::CustomEndpointConfigurationError => (
                Some("Fix agents.custom_endpoints in settings".to_owned()),
                Some("(Configuration error)".to_owned()),
                false,
            ),
            TuiApiKeysRowKind::CustomEndpoint(id) => {
                let suffix = if custom_endpoint_connected(id, ctx) {
                    "(Connected)"
                } else {
                    "(Not connected)"
                };
                (Some(String::new()), Some(suffix.to_owned()), !is_connecting)
            }
            TuiApiKeysRowKind::WarpCreditFallbackSetting => (
                Some(FALLBACK_DESCRIPTION.to_owned()),
                Some(format!(
                    "({})",
                    if *AISettings::as_ref(ctx).can_use_warp_credits_for_fallback {
                        "on"
                    } else {
                        "off"
                    }
                )),
                !is_connecting,
            ),
        };
        let style = if row.kind == TuiApiKeysRowKind::WarpCreditFallbackSetting {
            TuiInlineMenuRowStyle::StateWithDetail
        } else {
            TuiInlineMenuRowStyle::InlineMenuItem
        };
        TuiInlineMenuRow {
            title: row.title.clone(),
            prefix: None,
            description,
            state_suffix,
            promotional_suffix: None,
            is_selectable: is_selectable && row.is_selectable,
            style,
        }
    }

    fn edit_provider(&mut self, provider: LLMProvider, ctx: &mut ModelContext<Self>) {
        if provider == LLMProvider::Anthropic {
            if ApiKeyManager::as_ref(ctx).has_anthropic_subscription() {
                self.set_browsing_error(
                    "Claude is already connected. Press Ctrl-X to disconnect.".to_owned(),
                    ctx,
                );
                return;
            }
            self.open_anthropic_selector(ctx);
            return;
        }
        if provider == LLMProvider::Xai {
            if ApiKeyManager::as_ref(ctx).has_grok_subscription() {
                self.set_browsing_error(
                    "Grok is already connected. Press Ctrl-X to disconnect.".to_owned(),
                    ctx,
                );
            } else {
                self.start_grok_oauth(ctx);
            }
            return;
        }
        let key = provider
            .api_key(ApiKeyManager::as_ref(ctx).keys())
            .unwrap_or_default()
            .to_owned();
        self.set_input(&key, ctx);
        self.state = TuiApiKeysMenuState::EditingProvider {
            provider,
            error: None,
        };
        ctx.emit(TuiApiKeysMenuEvent);
    }

    fn save_provider(&mut self, provider: LLMProvider, ctx: &mut ModelContext<Self>) {
        let value = input_text(&self.input_editor, ctx);
        let value = (!value.is_empty()).then_some(value);
        match ApiKeyManager::handle(ctx).update(ctx, |manager, ctx| {
            manager.persist_provider_key(provider, value, ctx)
        }) {
            Ok(()) => self.transition_to_browsing(ctx),
            Err(_) => {
                if let TuiApiKeysMenuState::EditingProvider { error, .. } = &mut self.state {
                    *error = Some("Could not save this API key. Try again.".to_owned());
                }
                ctx.emit(TuiApiKeysMenuEvent);
            }
        }
    }

    fn edit_custom_endpoint(&mut self, id: CustomEndpointId, ctx: &mut ModelContext<Self>) {
        let Some(name) = ApiKeyManager::as_ref(ctx)
            .custom_endpoint_definitions()
            .and_then(|definitions| definitions.get(&id))
            .map(|definition| definition.name.clone())
        else {
            self.refresh_rows(ctx);
            return;
        };
        let key = ApiKeyManager::as_ref(ctx)
            .custom_endpoint_key(&id)
            .unwrap_or_default()
            .to_owned();
        self.set_input(&key, ctx);
        self.state = TuiApiKeysMenuState::EditingCustomEndpoint {
            id,
            name,
            error: None,
        };
        ctx.emit(TuiApiKeysMenuEvent);
    }

    fn save_custom_endpoint(&mut self, id: CustomEndpointId, ctx: &mut ModelContext<Self>) {
        let value = input_text(&self.input_editor, ctx);
        let value = (!value.is_empty()).then_some(value);
        match ApiKeyManager::handle(ctx).update(ctx, |manager, ctx| {
            manager.persist_custom_endpoint_key(id, value, ctx)
        }) {
            Ok(()) => self.transition_to_browsing(ctx),
            Err(_) => {
                if let TuiApiKeysMenuState::EditingCustomEndpoint { error, .. } = &mut self.state {
                    *error = Some("Could not save this API key. Try again.".to_owned());
                }
                ctx.emit(TuiApiKeysMenuEvent);
            }
        }
    }

    fn open_anthropic_selector(&mut self, ctx: &mut ModelContext<Self>) {
        let mut list = TuiInlineMenuListState::default();
        open_anthropic_selector_impl(&mut list);
        self.state = TuiApiKeysMenuState::AnthropicSelector { list };
        ctx.emit(TuiApiKeysMenuEvent);
    }
    fn toggle_fallback(&mut self, ctx: &mut ModelContext<Self>) {
        let result = AISettings::handle(ctx).update(ctx, |settings, ctx| {
            settings
                .can_use_warp_credits_for_fallback
                .toggle_and_save_value(ctx)
        });
        match result {
            Ok(_) => self.refresh_rows(ctx),
            Err(_) => self.set_browsing_error(
                "Could not save the Warp credit fallback setting.".to_owned(),
                ctx,
            ),
        }
    }

    fn transition_to_browsing(&mut self, ctx: &mut ModelContext<Self>) {
        if let TuiApiKeysMenuState::ConnectingGrok { controller } = &self.state
            && controller.as_ref(ctx).is_active()
        {
            controller.update(ctx, |controller, ctx| controller.cancel(ctx));
        }
        if let TuiApiKeysMenuState::ConnectingAnthropic { controller } = &self.state
            && controller.as_ref(ctx).is_active()
        {
            controller.update(ctx, |controller, ctx| controller.cancel(ctx));
        }
        if let TuiApiKeysMenuState::ConnectingChatGpt { controller } = &self.state
            && controller.as_ref(ctx).is_active()
        {
            controller.update(ctx, |controller, ctx| controller.cancel(ctx));
        }
        self.clear_input(ctx);
        self.state = TuiApiKeysMenuState::Browsing {
            list: TuiInlineMenuListState::default(),
            error: None,
        };
        self.refresh_rows(ctx);
    }

    fn close(&mut self, ctx: &mut ModelContext<Self>) {
        self.deactivate(ctx);
        self.suggestions_mode.update(ctx, |mode, ctx| {
            mode.close_if_active(TuiInputSuggestionsMode::ApiKeys, ctx);
        });
    }

    /// Clears API-key-specific state when shared menu arbitration moves elsewhere.
    fn deactivate(&mut self, ctx: &mut ModelContext<Self>) {
        if matches!(self.state, TuiApiKeysMenuState::Closed) {
            return;
        }
        let previous_state = std::mem::replace(&mut self.state, TuiApiKeysMenuState::Closed);
        match previous_state {
            TuiApiKeysMenuState::ConnectingGrok { controller } => {
                if controller.as_ref(ctx).is_active() {
                    controller.update(ctx, |controller, ctx| controller.cancel(ctx));
                }
            }
            TuiApiKeysMenuState::ConnectingAnthropic { controller } => {
                if controller.as_ref(ctx).is_active() {
                    controller.update(ctx, |controller, ctx| controller.cancel(ctx));
                }
            }
            TuiApiKeysMenuState::ConnectingChatGpt { controller } => {
                if controller.as_ref(ctx).is_active() {
                    controller.update(ctx, |controller, ctx| controller.cancel(ctx));
                }
            }
            TuiApiKeysMenuState::Closed
            | TuiApiKeysMenuState::Browsing { .. }
            | TuiApiKeysMenuState::EditingProvider { .. }
            | TuiApiKeysMenuState::EditingCustomEndpoint { .. }
            | TuiApiKeysMenuState::AnthropicSelector { .. } => {}
        }
        self.clear_input(ctx);
        ctx.emit(TuiApiKeysMenuEvent);
    }

    fn refresh_rows(&mut self, ctx: &mut ModelContext<Self>) {
        let TuiApiKeysMenuState::Browsing { list, .. } = &mut self.state else {
            ctx.emit(TuiApiKeysMenuEvent);
            return;
        };
        let query = input_text(&self.input_editor, ctx).to_ascii_lowercase();
        let rows = all_rows(ctx)
            .into_iter()
            .filter(|row| {
                row.kind == TuiApiKeysRowKind::WarpCreditFallbackSetting
                    || row.title.to_ascii_lowercase().contains(&query)
            })
            .collect();
        let previous_kind = list.selected_row().map(|row| row.kind.clone());
        let preferred_index = previous_kind
            .and_then(|kind| {
                let rows: &Vec<TuiApiKeysRow> = &rows;
                rows.iter().position(|row| row.kind == kind)
            })
            .or(Some(0));
        list.replace_rows(rows, false, preferred_index, MAX_VISIBLE_ROWS, |row| {
            row.is_selectable
        });
        ctx.emit(TuiApiKeysMenuEvent);
    }

    fn set_browsing_error(&mut self, message: String, ctx: &mut ModelContext<Self>) {
        if let TuiApiKeysMenuState::Browsing { error, .. } = &mut self.state {
            *error = Some(message);
            ctx.emit(TuiApiKeysMenuEvent);
        }
    }

    fn clear_input(&self, ctx: &mut ModelContext<Self>) {
        self.input_editor
            .update(ctx, |editor, ctx| editor.clear_buffer(ctx));
    }

    fn set_input(&self, text: &str, ctx: &mut ModelContext<Self>) {
        self.input_editor.update(ctx, |editor, ctx| {
            editor.clear_buffer(ctx);
            editor.user_insert(text, ctx);
        });
    }
}

fn custom_endpoint_connected(id: &CustomEndpointId, ctx: &AppContext) -> bool {
    ApiKeyManager::as_ref(ctx).custom_endpoint_key(id).is_some()
}

fn all_rows(ctx: &AppContext) -> Vec<TuiApiKeysRow> {
    // Organization builds' agent calls Anthropic and Cerebras only and has no Warp credits to
    // fall back on.
    if TuiLoginModel::is_organization_sign_in() {
        return vec![
            TuiApiKeysRow {
                kind: TuiApiKeysRowKind::Provider(LLMProvider::Anthropic),
                title: "Anthropic API key".to_owned(),
                is_selectable: true,
            },
            TuiApiKeysRow {
                kind: TuiApiKeysRowKind::ChatGptLogin,
                title: "ChatGPT plan (Codex sign-in)".to_owned(),
                is_selectable: true,
            },
            cerebras_row(),
        ];
    }
    let mut rows = vec![
        TuiApiKeysRow {
            kind: TuiApiKeysRowKind::Provider(LLMProvider::Anthropic),
            title: "Anthropic API key".to_owned(),
            is_selectable: true,
        },
        TuiApiKeysRow {
            kind: TuiApiKeysRowKind::Provider(LLMProvider::Google),
            title: "Google API key".to_owned(),
            is_selectable: true,
        },
        TuiApiKeysRow {
            kind: TuiApiKeysRowKind::Provider(LLMProvider::OpenAI),
            title: "OpenAI API key".to_owned(),
            is_selectable: true,
        },
        TuiApiKeysRow {
            kind: TuiApiKeysRowKind::Provider(LLMProvider::Xai),
            title: "X premium or SuperGrok subscription".to_owned(),
            is_selectable: true,
        },
        cerebras_row(),
    ];
    let manager = ApiKeyManager::as_ref(ctx);
    if !manager.custom_endpoint_settings_valid() {
        rows.push(TuiApiKeysRow {
            kind: TuiApiKeysRowKind::CustomEndpointConfigurationError,
            title: "Custom endpoints".to_owned(),
            is_selectable: false,
        });
    } else if let Some(definitions) = manager.custom_endpoint_definitions()
        && !definitions.is_empty()
    {
        let mut endpoint_rows = definitions
            .definitions()
            .map(|(id, definition)| TuiApiKeysRow {
                kind: TuiApiKeysRowKind::CustomEndpoint(id.clone()),
                title: format!("{} custom endpoint", definition.name),
                is_selectable: true,
            })
            .collect::<Vec<_>>();
        endpoint_rows.sort_by_key(|row| row.title.to_ascii_lowercase());
        rows.extend(endpoint_rows);
    }
    rows.push(TuiApiKeysRow {
        kind: TuiApiKeysRowKind::WarpCreditFallbackSetting,
        title: "Warp credit fallback".to_owned(),
        is_selectable: true,
    });
    rows
}
fn cerebras_row() -> TuiApiKeysRow {
    TuiApiKeysRow {
        kind: TuiApiKeysRowKind::Provider(LLMProvider::Cerebras),
        title: "Cerebras API key".to_owned(),
        is_selectable: true,
    }
}

fn row_index(rows: &[TuiApiKeysRow], kind: &TuiApiKeysRowKind) -> Option<usize> {
    rows.iter().position(|row| row.kind == *kind)
}

/// Returns whether the provider has a configured API key or connected subscription.
fn provider_connected(provider: LLMProvider, ctx: &AppContext) -> bool {
    if provider == LLMProvider::Xai {
        ApiKeyManager::as_ref(ctx).has_grok_subscription()
    } else if provider == LLMProvider::Anthropic {
        ApiKeyManager::as_ref(ctx).has_anthropic_subscription()
            || provider
                .api_key(ApiKeyManager::as_ref(ctx).keys())
                .is_some_and(|k| !k.is_empty())
    } else {
        provider
            .api_key(ApiKeyManager::as_ref(ctx).keys())
            .is_some_and(|key| !key.is_empty())
    }
}
fn anthropic_connected_suffix(ctx: &AppContext) -> String {
    let manager = ApiKeyManager::as_ref(ctx);
    if manager
        .keys()
        .anthropic
        .as_deref()
        .is_some_and(|key| !key.trim().is_empty())
    {
        return "Connected".to_owned();
    }
    let Some(tokens) = manager.anthropic_tokens() else {
        return "Not connected".to_owned();
    };
    // Warp's server has no request field for the OAuth token, so only a self-hosted server can run
    // Claude models with the login alone.
    let caveat = if manager.sends_anthropic_oauth_as_api_key() {
        ""
    } else {
        ", not yet used for requests"
    };
    match tokens.connected_at {
        Some(connected_at) => format!(
            "Connected on {}{caveat}",
            chrono::DateTime::<chrono::Local>::from(connected_at).format("%m/%d/%Y at %-I:%M%P")
        ),
        None => format!("Connected{caveat}"),
    }
}
fn open_anthropic_selector_impl(list: &mut TuiInlineMenuListState<AnthropicSelectorRow>) {
    let rows = anthropic_selector_rows();
    list.replace_rows(rows, false, Some(1), MAX_VISIBLE_ROWS, |_| true);
}

/// Returns the shared input editor's current text.
fn input_text(editor: &ModelHandle<CodeEditorModel>, app: &AppContext) -> String {
    let model = editor.as_ref(app);
    let buffer = model.content().as_ref(app);
    if buffer.is_empty() {
        String::new()
    } else {
        buffer.text().into_string()
    }
}

/// Renders the footer actions for the current API keys menu state.
pub(crate) fn render_api_keys_footer(
    footer: TuiApiKeysFooter,
    builder: &TuiUiBuilder,
) -> Box<dyn TuiElement> {
    let key = builder.link_text_style();
    let muted = builder.muted_text_style();
    let accent = builder.credential_entry_accent_style();
    let spans = match footer {
        TuiApiKeysFooter::ProviderList { can_clear } => {
            let mut spans = vec![
                ("enter".to_owned(), key),
                (" to set api key | ".to_owned(), muted),
            ];
            if can_clear {
                spans.extend([
                    ("ctrl + x".to_owned(), key),
                    (" to clear api key | ".to_owned(), muted),
                ]);
            }
            spans.extend([
                ("esc".to_owned(), key),
                (" to close menu".to_owned(), muted),
            ]);
            spans
        }
        TuiApiKeysFooter::WarpCreditFallback => vec![
            ("enter".to_owned(), key),
            (" to toggle warp credit fallback | ".to_owned(), muted),
            ("esc".to_owned(), key),
            (" to close menu".to_owned(), muted),
        ],
        TuiApiKeysFooter::EditingProvider(provider) => vec![
            (
                format!("Connect {} API key", provider.display_name()),
                accent,
            ),
            (" | ".to_owned(), muted),
            ("enter".to_owned(), key),
            (" to save key | ".to_owned(), muted),
            ("esc".to_owned(), key),
            (" to cancel".to_owned(), muted),
        ],
        TuiApiKeysFooter::EditingCustomEndpoint(name) => vec![
            (format!("Connect {name} API key"), accent),
            (" | ".to_owned(), muted),
            ("enter".to_owned(), key),
            (" to save key | ".to_owned(), muted),
            ("esc".to_owned(), key),
            (" to cancel".to_owned(), muted),
        ],
        TuiApiKeysFooter::ConnectingGrok => vec![
            ("Connect X premium/SuperGrok".to_owned(), accent),
            (" | ".to_owned(), muted),
            ("enter".to_owned(), key),
            (" to confirm | ".to_owned(), muted),
            ("esc".to_owned(), key),
            (" to cancel".to_owned(), muted),
        ],
        TuiApiKeysFooter::AnthropicSelector => vec![
            ("Anthropic".to_owned(), accent),
            (" | ".to_owned(), muted),
            ("enter".to_owned(), key),
            (" to select | ".to_owned(), muted),
            ("esc".to_owned(), key),
            (" to cancel".to_owned(), muted),
        ],
        TuiApiKeysFooter::ConnectingAnthropic => vec![
            ("Connect Claude".to_owned(), accent),
            (" | ".to_owned(), muted),
            ("enter".to_owned(), key),
            (" to confirm | ".to_owned(), muted),
            ("esc".to_owned(), key),
            (" to cancel".to_owned(), muted),
        ],
        TuiApiKeysFooter::ConnectingChatGpt => vec![
            ("Connect ChatGPT".to_owned(), accent),
            (" | approve the code in your browser | ".to_owned(), muted),
            ("esc".to_owned(), key),
            (" to cancel".to_owned(), muted),
        ],
    };
    TuiText::from_spans(spans).truncate().finish()
}
impl Entity for TuiApiKeysMenuModel {
    type Event = TuiApiKeysMenuEvent;
}

#[cfg(test)]
#[path = "api_keys_menu_tests.rs"]
mod tests;
