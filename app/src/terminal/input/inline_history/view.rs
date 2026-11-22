//! Inline history menu view for up-arrow history with conversations, commands and prompts.
use std::collections::HashSet;

use pathfinder_color::ColorU;
use settings::Setting as _;
use warp_core::ui::appearance::Appearance;
use warp_core::ui::theme::Fill;
use warp_core::ui::theme::color::internal_colors;
use warp_errors::report_if_error;
use warpui::elements::{ChildView, Flex, ParentElement};
use warpui::{
    AppContext, Element, Entity, EntityId, ModelHandle, SingletonEntity, View, ViewContext,
    ViewHandle,
};

use crate::ai::agent::conversation::AIConversationId;
use crate::ai::blocklist::agent_view::{AgentViewController, AgentViewControllerEvent};
use crate::features::FeatureFlag;
use crate::search::data_source::{Query, QueryFilter};
use crate::search::mixer::{SearchMixer, SearchMixerEvent};
use crate::settings::{InputSettings, InputSettingsChangedEvent};
use crate::settings_view::SettingsSection;
use crate::terminal::history::{HistoryScope, LinkedWorkflowData};
use crate::terminal::input::InputAction;
use crate::terminal::input::buffer_model::{InputBufferModel, InputBufferUpdateEvent};
use crate::terminal::input::inline_history::data_source::{
    AcceptHistoryItem, InlineHistoryMenuDataSource,
};
use crate::terminal::input::inline_menu::{
    InlineMenuEvent, InlineMenuHeaderConfig, InlineMenuModel, InlineMenuPositioner,
    InlineMenuTabConfig, InlineMenuView,
};
use crate::terminal::input::suggestions_mode_model::{
    InputSuggestionsModeEvent, InputSuggestionsModeModel,
};
use crate::terminal::model::session::active_session::ActiveSession;
use crate::ui_components::icons::Icon;
use crate::view_components::action_button::{ActionButton, ActionButtonTheme, ButtonSize};
use crate::workspace::WorkspaceAction;

#[derive(Debug, Clone)]
pub enum InlineHistoryMenuEvent {
    NavigateToConversation {
        conversation_id: AIConversationId,
    },
    AcceptCommand {
        command: String,
        linked_workflow_data: Option<LinkedWorkflowData>,
    },
    AcceptAIPrompt {
        query_text: String,
    },
    SelectCommand {
        command: String,
        linked_workflow_data: Option<LinkedWorkflowData>,
    },
    SelectAIPrompt {
        query_text: String,
    },
    /// Emitted when a conversation row becomes selected so any previously
    /// previewed command or prompt text is cleared from the input buffer.
    SelectConversation,
    NoResults,
    /// Emitted when the inline menu should be closed and additionally restore the
    /// original input buffer contents.
    Close,
}

#[derive(Clone)]
/// Identifies a history item well enough to reselect the same logical item
/// after rerunning the current query.
enum HistoryItemIdentity {
    Conversation(AIConversationId),
    Command(String),
    AIPrompt(String),
}

impl HistoryItemIdentity {
    fn from_item(item: &AcceptHistoryItem) -> Self {
        match item {
            AcceptHistoryItem::Conversation {
                conversation_id, ..
            } => Self::Conversation(*conversation_id),
            AcceptHistoryItem::Command { command, .. } => Self::Command(command.clone()),
            AcceptHistoryItem::AIPrompt { query_text } => Self::AIPrompt(query_text.clone()),
        }
    }

    fn matches(&self, item: &AcceptHistoryItem) -> bool {
        match (self, item) {
            (
                Self::Conversation(expected_id),
                AcceptHistoryItem::Conversation {
                    conversation_id, ..
                },
            ) => *expected_id == *conversation_id,
            (Self::Command(expected_command), AcceptHistoryItem::Command { command, .. }) => {
                expected_command == command
            }
            (Self::AIPrompt(expected_query), AcceptHistoryItem::AIPrompt { query_text }) => {
                expected_query == query_text
            }
            _ => false,
        }
    }
}

struct ConfigureButtonTheme;

impl ActionButtonTheme for ConfigureButtonTheme {
    fn background(&self, _hovered: bool, _appearance: &Appearance) -> Option<Fill> {
        None
    }

    fn text_color(
        &self,
        hovered: bool,
        _background: Option<Fill>,
        appearance: &Appearance,
    ) -> ColorU {
        let theme = appearance.theme();
        if hovered {
            internal_colors::text_main(theme, theme.background())
        } else {
            internal_colors::text_sub(theme, theme.background())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryTab {
    All,
    Commands,
    Prompts,
    Scope(HistoryScope),
}

/// Outside the agent view, the history menu's tabs select the [`HistoryScope`]. Inside it, the
/// tabs select the item type and the scope comes from the persisted setting.
fn build_tab_configs(is_agent_view: bool) -> Vec<InlineMenuTabConfig<HistoryTab>> {
    if !is_agent_view {
        return HistoryScope::ALL
            .into_iter()
            .map(|scope| InlineMenuTabConfig {
                id: HistoryTab::Scope(scope),
                label: scope.label().to_string(),
                filters: HashSet::new(),
            })
            .collect();
    }

    vec![
        InlineMenuTabConfig {
            id: HistoryTab::All,
            label: "All".to_string(),
            filters: HashSet::new(),
        },
        InlineMenuTabConfig {
            id: HistoryTab::Commands,
            label: "Commands".to_string(),
            filters: HashSet::from([QueryFilter::Commands]),
        },
        InlineMenuTabConfig {
            id: HistoryTab::Prompts,
            label: "Prompts".to_string(),
            filters: HashSet::from([QueryFilter::PromptHistory]),
        },
    ]
}

/// Label of the header's scope selector, which names the persisted scope.
pub(super) fn scope_selector_label(scope: HistoryScope) -> String {
    format!("Scope: {}", scope.label())
}

/// Whether the tabs select the item type, so scope needs its own selector in the header.
pub(super) fn tabs_select_item_type(tab_configs: &[InlineMenuTabConfig<HistoryTab>]) -> bool {
    !tab_configs
        .iter()
        .any(|config| matches!(config.id, HistoryTab::Scope(_)))
}

pub struct InlineHistoryMenuView {
    menu_view: ViewHandle<InlineMenuView<AcceptHistoryItem, HistoryTab>>,
    mixer: ModelHandle<SearchMixer<AcceptHistoryItem>>,
    model: ModelHandle<InlineMenuModel<AcceptHistoryItem, HistoryTab>>,
    buffer_model: ModelHandle<InputBufferModel>,
    pending_tab_switch_selection: Option<HistoryItemIdentity>,
    caller_supplied_tabs: bool,
    pending_initial_buffer_sync: bool,
    scope_selector: Option<ViewHandle<ActionButton>>,
}

impl InlineHistoryMenuView {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        terminal_view_id: EntityId,
        active_session: ModelHandle<ActiveSession>,
        input_suggestions_model: &ModelHandle<InputSuggestionsModeModel>,
        agent_view_controller: ModelHandle<AgentViewController>,
        positioner: &ModelHandle<InlineMenuPositioner>,
        buffer_model: ModelHandle<InputBufferModel>,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let is_agent_view = agent_view_controller.as_ref(ctx).is_active();
        let tab_configs = build_tab_configs(is_agent_view);
        Self::new_inner(
            terminal_view_id,
            active_session,
            input_suggestions_model,
            agent_view_controller,
            positioner,
            buffer_model,
            tab_configs,
            /* caller_supplied_tabs */ false,
            ctx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_tab_configs(
        terminal_view_id: EntityId,
        active_session: ModelHandle<ActiveSession>,
        input_suggestions_model: &ModelHandle<InputSuggestionsModeModel>,
        agent_view_controller: ModelHandle<AgentViewController>,
        positioner: &ModelHandle<InlineMenuPositioner>,
        buffer_model: ModelHandle<InputBufferModel>,
        tab_configs: Vec<InlineMenuTabConfig<HistoryTab>>,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        Self::new_inner(
            terminal_view_id,
            active_session,
            input_suggestions_model,
            agent_view_controller,
            positioner,
            buffer_model,
            tab_configs,
            /* caller_supplied_tabs */ true,
            ctx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_inner(
        terminal_view_id: EntityId,
        active_session: ModelHandle<ActiveSession>,
        input_suggestions_model: &ModelHandle<InputSuggestionsModeModel>,
        agent_view_controller: ModelHandle<AgentViewController>,
        positioner: &ModelHandle<InlineMenuPositioner>,
        buffer_model: ModelHandle<InputBufferModel>,
        tab_configs: Vec<InlineMenuTabConfig<HistoryTab>>,
        caller_supplied_tabs: bool,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let data_source = ctx.add_model(|_| {
            InlineHistoryMenuDataSource::new(
                terminal_view_id,
                active_session,
                agent_view_controller.clone(),
            )
        });

        let initial_tab = HistoryTab::Scope(*InputSettings::as_ref(ctx).history_scope);
        let initial_filters = tab_configs
            .iter()
            .find(|config| config.id == initial_tab)
            .or(tab_configs.first())
            .map(|config| config.filters.clone())
            .unwrap_or_default();

        let mixer = ctx.add_model(|ctx| {
            let mut mixer = SearchMixer::<AcceptHistoryItem>::new();
            mixer.add_sync_source(
                data_source,
                [
                    QueryFilter::Commands,
                    QueryFilter::Conversations,
                    QueryFilter::PromptHistory,
                ],
            );
            mixer.run_query(
                Query {
                    text: String::new(),
                    filters: initial_filters,
                },
                ctx,
            );
            mixer
        });

        let mut scope_selector = None;
        let menu_view = if FeatureFlag::InlineMenuHeaders.is_enabled() {
            let selector = ctx.add_view(|ctx| {
                ActionButton::new(
                    scope_selector_label(*InputSettings::as_ref(ctx).history_scope),
                    ConfigureButtonTheme,
                )
                .with_size(ButtonSize::Small)
                .with_menu(true)
                .with_tooltip("Cycle history scope (ctrl-s)")
                .on_click(|ctx| ctx.dispatch_typed_action(InputAction::CycleHistoryScope))
            });
            scope_selector = Some(selector.clone());
            let header_model = std::cell::OnceCell::<
                ModelHandle<InlineMenuModel<AcceptHistoryItem, HistoryTab>>,
            >::new();
            let header_model = std::rc::Rc::new(header_model);
            let header_model_for_render = header_model.clone();
            let configure_button = ctx.add_view(|_| {
                ActionButton::new("Configure", ConfigureButtonTheme)
                    .with_icon(Icon::Settings)
                    .with_size(ButtonSize::Small)
                    .on_click(|ctx| {
                        ctx.dispatch_typed_action(WorkspaceAction::ShowSettingsPageWithSearch {
                            search_query: "commands history".into(),
                            section: Some(SettingsSection::WarpAgent),
                        });
                    })
            });
            let header_config = InlineMenuHeaderConfig {
                label: "History".to_string(),
                trailing_element: Some(Box::new(move |app: &AppContext| {
                    let configure = ChildView::new(&configure_button).finish();
                    let shows_selector = header_model_for_render.get().is_some_and(|model| {
                        tabs_select_item_type(model.as_ref(app).tab_configs())
                    });
                    if !shows_selector {
                        return configure;
                    }
                    Flex::row()
                        .with_child(ChildView::new(&selector).finish())
                        .with_child(configure)
                        .finish()
                })),
            };
            let menu_view = ctx.add_typed_action_view(|ctx| {
                InlineMenuView::new_with_tabs(
                    mixer.clone(),
                    positioner.clone(),
                    input_suggestions_model,
                    agent_view_controller.clone(),
                    tab_configs,
                    Some(initial_tab),
                    ctx,
                )
                .with_header_config(header_config)
            });
            let _ = header_model.set(menu_view.as_ref(ctx).model().clone());
            menu_view
        } else {
            ctx.add_typed_action_view(|ctx| {
                InlineMenuView::new_with_tabs(
                    mixer.clone(),
                    positioner.clone(),
                    input_suggestions_model,
                    agent_view_controller.clone(),
                    tab_configs,
                    Some(initial_tab),
                    ctx,
                )
            })
        };
        let model = menu_view.as_ref(ctx).model().clone();

        ctx.subscribe_to_model(input_suggestions_model, |me, model, event, ctx| {
            let InputSuggestionsModeEvent::ModeChanged { .. } = event;
            if model.as_ref(ctx).is_inline_history_menu() {
                me.open_with_current_buffer(ctx);
            }
        });

        let suggestions_mode_model_for_buffer = input_suggestions_model.clone();
        ctx.subscribe_to_model(
            &buffer_model,
            move |me, _, _: &InputBufferUpdateEvent, ctx| {
                if !suggestions_mode_model_for_buffer
                    .as_ref(ctx)
                    .is_inline_history_menu()
                {
                    return;
                }
                if !me.pending_initial_buffer_sync {
                    return;
                }
                me.pending_initial_buffer_sync = false;
                me.open_with_current_buffer(ctx);
            },
        );

        let suggestions_mode_model = input_suggestions_model.clone();
        ctx.subscribe_to_model(
            &agent_view_controller,
            move |me, controller, event, ctx| match event {
                AgentViewControllerEvent::EnteredAgentView { .. }
                | AgentViewControllerEvent::ExitedAgentView { .. } => {
                    // Only auto-rebuild tabs from `is_agent_view` when the
                    // caller did not supply tabs explicitly. Callers that
                    // pinned tabs (e.g. the cloud-mode V2 wrapper) want their
                    // tab set preserved across agent-view enter/exit.
                    if !me.caller_supplied_tabs {
                        let is_agent_view = controller.as_ref(ctx).is_active();
                        let new_configs = build_tab_configs(is_agent_view);
                        let scope_tab =
                            HistoryTab::Scope(*InputSettings::as_ref(ctx).history_scope);
                        me.model.update(ctx, |model, _| {
                            model.set_tab_configs(new_configs);
                            model.set_active_tab_id(&scope_tab);
                        });
                        if suggestions_mode_model.as_ref(ctx).is_inline_history_menu() {
                            me.pending_tab_switch_selection = me
                                .model
                                .as_ref(ctx)
                                .selected_item()
                                .map(HistoryItemIdentity::from_item);
                            me.rerun_query(ctx);
                        }
                    }
                    me.menu_view.update(ctx, |_, ctx| ctx.notify());
                }
                AgentViewControllerEvent::ExitConfirmed { .. } => {}
            },
        );

        ctx.subscribe_to_view(&menu_view, |me, _, event, ctx| match event {
            InlineMenuEvent::AcceptedItem { item, .. } => match item {
                AcceptHistoryItem::Conversation {
                    conversation_id, ..
                } => {
                    ctx.emit(InlineHistoryMenuEvent::NavigateToConversation {
                        conversation_id: *conversation_id,
                    });
                }
                AcceptHistoryItem::Command {
                    command,
                    linked_workflow_data,
                } => {
                    ctx.emit(InlineHistoryMenuEvent::AcceptCommand {
                        command: command.clone(),
                        linked_workflow_data: linked_workflow_data.clone(),
                    });
                }
                AcceptHistoryItem::AIPrompt { query_text } => {
                    ctx.emit(InlineHistoryMenuEvent::AcceptAIPrompt {
                        query_text: query_text.clone(),
                    });
                }
            },
            InlineMenuEvent::SelectedItem { item } => match item {
                AcceptHistoryItem::Command {
                    command,
                    linked_workflow_data,
                } => {
                    ctx.emit(InlineHistoryMenuEvent::SelectCommand {
                        command: command.clone(),
                        linked_workflow_data: linked_workflow_data.clone(),
                    });
                }
                AcceptHistoryItem::AIPrompt { query_text } => {
                    ctx.emit(InlineHistoryMenuEvent::SelectAIPrompt {
                        query_text: query_text.clone(),
                    });
                }
                AcceptHistoryItem::Conversation { .. } => {
                    ctx.emit(InlineHistoryMenuEvent::SelectConversation);
                }
            },
            InlineMenuEvent::Dismissed => {
                ctx.emit(InlineHistoryMenuEvent::Close);
            }
            InlineMenuEvent::NoResults => {
                ctx.emit(InlineHistoryMenuEvent::NoResults);
            }
            InlineMenuEvent::TabChanged => {
                if let Some(HistoryTab::Scope(scope)) = me.model.as_ref(ctx).active_tab_id() {
                    persist_history_scope(scope, ctx);
                }
                me.pending_tab_switch_selection = me
                    .model
                    .as_ref(ctx)
                    .selected_item()
                    .map(HistoryItemIdentity::from_item);
                me.rerun_query(ctx);
            }
        });

        ctx.subscribe_to_model(&InputSettings::handle(ctx), |me, _, event, ctx| {
            if matches!(event, InputSettingsChangedEvent::HistoryScopeSetting { .. }) {
                me.sync_scope_selector_label(ctx);
            }
        });

        let suggestions_mode_model = input_suggestions_model.clone();
        ctx.subscribe_to_model(&mixer, move |me, _, event, ctx| {
            let SearchMixerEvent::ResultsChanged = event;
            if !suggestions_mode_model.as_ref(ctx).is_inline_history_menu() {
                return;
            }
            // Only tab switches stash a pending selection to restore after the
            // query reruns. For all other result updates, keep the inline menu's
            // default selection behavior.
            let Some(selection) = me.pending_tab_switch_selection.take() else {
                return;
            };
            me.menu_view.update(ctx, |menu, ctx| {
                menu.select_last_where(|item| selection.matches(item), ctx);
            });
        });

        Self {
            menu_view,
            mixer,
            model,
            buffer_model,
            pending_tab_switch_selection: None,
            caller_supplied_tabs,
            pending_initial_buffer_sync: false,
            scope_selector,
        }
    }

    /// Advances the persisted [`HistoryScope`] and reruns the query so the command rows reflect
    /// it. Prompt and conversation rows are not scoped.
    pub fn cycle_scope(&mut self, ctx: &mut ViewContext<Self>) {
        let scope = InputSettings::as_ref(ctx).history_scope.next();
        persist_history_scope(scope, ctx);
        let scope_tab = HistoryTab::Scope(scope);
        self.model
            .update(ctx, |model, _| model.set_active_tab_id(&scope_tab));
        self.sync_scope_selector_label(ctx);
        self.pending_tab_switch_selection = self
            .model
            .as_ref(ctx)
            .selected_item()
            .map(HistoryItemIdentity::from_item);
        self.rerun_query(ctx);
        self.menu_view.update(ctx, |_, ctx| ctx.notify());
    }

    fn sync_scope_selector_label(&self, ctx: &mut ViewContext<Self>) {
        let Some(selector) = &self.scope_selector else {
            return;
        };
        let label = scope_selector_label(*InputSettings::as_ref(ctx).history_scope);
        selector.update(ctx, |button, ctx| button.set_label(label, ctx));
    }

    /// Returns the model handle for external use (e.g., by message bars).
    pub fn model(&self) -> &ModelHandle<InlineMenuModel<AcceptHistoryItem, HistoryTab>> {
        &self.model
    }

    pub fn render_results_only(&self, app: &AppContext) -> Box<dyn Element> {
        self.menu_view.as_ref(app).render_results_only(
            /* should_render_results_in_reverse */ false, /* horizontal_padding */ 0.,
            app,
        )
    }

    pub fn result_count(&self, app: &AppContext) -> usize {
        self.menu_view.as_ref(app).result_count()
    }

    pub fn select_up(&self, ctx: &mut ViewContext<Self>) {
        self.menu_view.update(ctx, |v, ctx| v.select_up(ctx));
    }

    pub fn select_down(&self, ctx: &mut ViewContext<Self>) {
        let should_close = self.menu_view.read(ctx, |v, _| {
            let result_count = v.result_count();
            let is_last_item_selected =
                result_count > 0 && v.selected_idx().is_some_and(|idx| idx == result_count - 1);
            is_last_item_selected || result_count == 0
        });

        if should_close {
            ctx.emit(InlineHistoryMenuEvent::Close);
        } else {
            self.menu_view.update(ctx, |v, ctx| v.select_down(ctx));
        }
    }

    pub fn select_next_tab(&self, ctx: &mut ViewContext<Self>) -> bool {
        self.menu_view.update(ctx, |v, ctx| v.select_next_tab(ctx))
    }

    pub fn accept_selected_item(&self, ctx: &mut ViewContext<Self>) {
        self.menu_view
            .update(ctx, |v, ctx| v.accept_selected_item(false, ctx));
    }

    pub fn arm_initial_buffer_sync(&mut self) {
        self.pending_initial_buffer_sync = true;
    }

    fn open_with_current_buffer(&mut self, ctx: &mut ViewContext<Self>) {
        // Another terminal's menu may have changed the persisted scope since this menu last opened.
        let scope_tab = HistoryTab::Scope(*InputSettings::as_ref(ctx).history_scope);
        self.model
            .update(ctx, |model, _| model.set_active_tab_id(&scope_tab));
        let query_text = self.buffer_model.as_ref(ctx).current_value().to_owned();
        let filters = self.model.as_ref(ctx).active_tab_filters();
        self.mixer.update(ctx, |mixer, ctx| {
            mixer.run_query(
                Query {
                    text: query_text,
                    filters,
                },
                ctx,
            );
        });
    }

    fn rerun_query(&self, ctx: &mut ViewContext<Self>) {
        let filters = self.model.as_ref(ctx).active_tab_filters();
        let query_text = self.current_query_text(ctx);
        self.mixer.update(ctx, |mixer, ctx| {
            mixer.run_query(
                Query {
                    text: query_text,
                    filters,
                },
                ctx,
            );
        });
    }

    fn current_query_text(&self, ctx: &ViewContext<Self>) -> String {
        // Read the query text from the mixer rather than the editor buffer. While the
        // inline history menu is open, moving the highlighted row can preview that
        // item in the editor buffer. When we rerun the search after a tab change, we
        // want to preserve the user's typed query, not the temporary preview text.
        self.mixer
            .as_ref(ctx)
            .current_query()
            .map(|q| q.text.clone())
            .unwrap_or_default()
    }
}

fn persist_history_scope(scope: HistoryScope, ctx: &mut AppContext) {
    InputSettings::handle(ctx).update(ctx, |settings, ctx| {
        if *settings.history_scope != scope {
            report_if_error!(settings.history_scope.set_value(scope, ctx));
        }
    });
}

impl View for InlineHistoryMenuView {
    fn ui_name() -> &'static str {
        "InlineHistoryMenuView"
    }

    fn render(&self, _app: &warpui::AppContext) -> Box<dyn Element> {
        ChildView::new(&self.menu_view).finish()
    }
}

impl Entity for InlineHistoryMenuView {
    type Event = InlineHistoryMenuEvent;
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
