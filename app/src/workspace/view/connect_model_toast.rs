use ai::api_keys::ApiKeyManager;
use settings::Setting as _;
use warpui::{SingletonEntity as _, ViewContext};

use super::Workspace;
use crate::auth::AuthStateProvider;
use crate::settings::AISettings;
use crate::settings_view::SettingsSection;
use crate::view_components::{DismissibleToast, ToastLink};
use crate::workspace::WorkspaceAction;

const TOAST_OBJECT_ID: &str = "connect_model";

/// Whether the user can run the agent on some model: a provider or custom-endpoint key, or a
/// connected Claude, ChatGPT, or Grok login.
pub(crate) fn has_model_credential(manager: &ApiKeyManager) -> bool {
    manager.has_any_key() || manager.has_anthropic_subscription()
}

/// Whether to suggest connecting a model: once per install, only in organization builds, and only
/// while no credential exists.
pub(crate) fn should_show_connect_model_toast(
    is_organization_sign_in: bool,
    is_logged_in: bool,
    has_credential: bool,
    already_shown: bool,
) -> bool {
    is_organization_sign_in && is_logged_in && !has_credential && !already_shown
}

/// The action that opens the same settings page as `/api-keys`.
pub(crate) fn open_api_keys_action() -> WorkspaceAction {
    WorkspaceAction::ShowSettingsPageWithSearch {
        search_query: "API keys".to_owned(),
        section: Some(SettingsSection::WarpAgent),
    }
}

impl Workspace {
    /// Shows the one-time "connect a model" toast when it applies, and dismisses it as soon as a
    /// credential is connected.
    pub(super) fn maybe_show_connect_model_toast(&mut self, ctx: &mut ViewContext<Self>) {
        // Checked first so non-organization builds never touch the singletons below.
        if !crate::auth::is_organization_sign_in() {
            return;
        }
        let api_key_manager = ApiKeyManager::handle(ctx);
        if !should_show_connect_model_toast(
            crate::auth::is_organization_sign_in(),
            AuthStateProvider::as_ref(ctx).get().is_logged_in(),
            has_model_credential(api_key_manager.as_ref(ctx)),
            *AISettings::as_ref(ctx).connect_model_toast_shown,
        ) {
            return;
        }

        AISettings::handle(ctx).update(ctx, |settings, ctx| {
            if let Err(error) = settings.connect_model_toast_shown.set_value(true, ctx) {
                log::warn!("Failed to record that the connect-a-model toast was shown: {error}");
            }
        });

        let toast = DismissibleToast::default(
            "Connect a model to use the agent: sign in with Claude or ChatGPT, or add an API key. \
             You can also type /api-keys."
                .to_owned(),
        )
        .with_object_id(TOAST_OBJECT_ID.to_owned())
        .with_link(
            ToastLink::new("Connect a model".to_owned())
                .with_onclick_action(open_api_keys_action()),
        );
        self.toast_stack.update(ctx, |toast_stack, ctx| {
            toast_stack.add_persistent_toast(toast, ctx);
        });

        ctx.subscribe_to_model(&api_key_manager, |me, manager, _, ctx| {
            if has_model_credential(manager.as_ref(ctx)) {
                me.toast_stack.update(ctx, |toast_stack, ctx| {
                    toast_stack.dismiss_older_toasts(TOAST_OBJECT_ID, ctx);
                });
            }
        });
    }
}

#[cfg(test)]
#[path = "connect_model_toast_tests.rs"]
mod tests;
