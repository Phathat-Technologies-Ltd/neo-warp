use ai::anthropic_subscription::oauth::ManualCodeExchange;
use warpui_core::App;

use super::{
    EMPTY_CODE_MESSAGE, MANUAL_FAILURE_MESSAGE, TuiAnthropicOAuthController, TuiAnthropicOAuthPhase,
};

fn test_controller() -> TuiAnthropicOAuthController {
    TuiAnthropicOAuthController {
        manual_exchange: Some(ManualCodeExchange {
            verifier: "verifier".to_owned(),
        }),
        phase: TuiAnthropicOAuthPhase::Waiting { manual_error: None },
        active: true,
    }
}

#[test]
fn empty_code_is_rejected_without_exchanging() {
    App::test((), |mut app| async move {
        let controller = app.add_model(|_| test_controller());
        controller.update(&mut app, |controller, ctx| {
            controller.submit_manual_code("   ".to_owned(), ctx);
            assert_eq!(controller.error(), Some(EMPTY_CODE_MESSAGE));
            assert!(matches!(
                controller.phase,
                TuiAnthropicOAuthPhase::Waiting { .. }
            ));

            controller.clear_manual_error(ctx);
            assert!(controller.error().is_none());
        });
    });
}

#[test]
fn manual_failure_is_sanitized_and_allows_retry() {
    App::test((), |mut app| async move {
        let controller = app.add_model(|_| test_controller());
        controller.update(&mut app, |controller, ctx| {
            controller.phase = TuiAnthropicOAuthPhase::ExchangingManualCode;
            controller.handle_manual_result(Err(anyhow::anyhow!("raw-secret")), ctx);
            assert_eq!(controller.error(), Some(MANUAL_FAILURE_MESSAGE));
            assert!(controller.is_active());
            assert!(matches!(
                controller.phase,
                TuiAnthropicOAuthPhase::Waiting { .. }
            ));
        });
    });
}

#[test]
fn cancellation_ignores_late_results() {
    App::test((), |mut app| async move {
        let controller = app.add_model(|_| test_controller());
        controller.update(&mut app, |controller, ctx| {
            controller.cancel(ctx);
            controller.handle_manual_result(Err(anyhow::anyhow!("stale")), ctx);
            assert!(!controller.is_active());
            assert!(controller.error().is_none());

            controller.submit_manual_code("CODE#STATE".to_owned(), ctx);
            assert!(controller.error().is_none());
        });
    });
}
