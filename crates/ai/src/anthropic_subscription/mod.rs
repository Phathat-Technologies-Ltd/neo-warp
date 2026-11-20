pub mod oauth;

use std::time::{Duration, SystemTime};

use futures::channel::oneshot;
use warp_errors::report_error;
use warpui_core::ModelContext;
use warpui_core::r#async::Timer;

use self::oauth::TokenResponse;
use crate::api_keys::{AnthropicRefreshOutcome, AnthropicTokens, ApiKeyManager};

const REFRESH_LEAD_TIME: Duration = Duration::from_secs(5 * 60);

pub fn anthropic_tokens_from_response(
    response: TokenResponse,
    previous: Option<&AnthropicTokens>,
) -> AnthropicTokens {
    let expires_at = response
        .expires_in
        .and_then(|s| u64::try_from(s).ok())
        .and_then(|s| SystemTime::now().checked_add(Duration::from_secs(s)));
    AnthropicTokens {
        access_token: response.access_token,
        refresh_token: response
            .refresh_token
            .or_else(|| previous.and_then(|t| t.refresh_token.clone())),
        expires_at,
        connected_at: previous
            .and_then(|t| t.connected_at)
            .or_else(|| Some(SystemTime::now())),
    }
}

impl ApiKeyManager {
    pub fn store_anthropic_tokens(&mut self, r: TokenResponse, ctx: &mut ModelContext<Self>) {
        apply_anthropic_tokens(self, r, ctx);
    }

    pub fn set_anthropic_refresh_allowed(&mut self, allowed: bool, ctx: &mut ModelContext<Self>) {
        if self.anthropic_refresh_allowed == allowed {
            return;
        }
        self.anthropic_refresh_allowed = allowed;
        if allowed {
            schedule_anthropic_refresh(self, ctx);
        }
    }

    pub(crate) fn anthropic_expired_refresh_token(&self, byo_allowed: bool) -> Option<String> {
        if !byo_allowed { return None; }
        let t = self.anthropic_tokens()?;
        if !t.is_expired() { return None; }
        t.refresh_token.clone()
    }

    pub fn begin_expired_anthropic_refresh(
        &mut self, byo_allowed: bool, ctx: &mut ModelContext<Self>,
    ) -> Option<oneshot::Receiver<AnthropicRefreshOutcome>> {
        self.set_anthropic_refresh_allowed(byo_allowed, ctx);
        let rt = self.anthropic_expired_refresh_token(byo_allowed)?;
        let (tx, rx) = oneshot::channel();
        spawn_anthropic_refresh(self, rt, vec![tx], ctx);
        Some(rx)
    }
}

fn apply_anthropic_tokens(m: &mut ApiKeyManager, r: TokenResponse, ctx: &mut ModelContext<ApiKeyManager>) {
    let t = anthropic_tokens_from_response(r, m.anthropic_tokens());
    m.set_anthropic_tokens(Some(t), ctx);
    schedule_anthropic_refresh(m, ctx);
}

fn schedule_anthropic_refresh(m: &mut ApiKeyManager, ctx: &mut ModelContext<ApiKeyManager>) {
    if !m.anthropic_refresh_allowed { return; }
    let Some(t) = m.anthropic_tokens().cloned() else { return; };
    let Some(rt) = t.refresh_token.clone() else { return; };
    let Some(exp) = t.expires_at else { return; };
    let now = SystemTime::now();
    let fire = exp.checked_sub(REFRESH_LEAD_TIME).unwrap_or(now);
    let delay = fire.duration_since(now).unwrap_or(Duration::ZERO);
    ctx.spawn(async move { Timer::after(delay).await; }, move |mm, _, ctx| {
        if !mm.anthropic_refresh_allowed { return; }
        let cur = mm.anthropic_tokens().and_then(|t| t.refresh_token.as_deref()) == Some(rt.as_str());
        if cur { spawn_anthropic_refresh(mm, rt, Vec::new(), ctx); }
    });
}

fn spawn_anthropic_refresh(
    m: &mut ApiKeyManager, rt: String, waiters: Vec<oneshot::Sender<AnthropicRefreshOutcome>>, ctx: &mut ModelContext<ApiKeyManager>,
) {
    if let Some(e) = m.anthropic_refresh_waiters.as_mut() { e.extend(waiters); return; }
    m.anthropic_refresh_waiters = Some(waiters);
    ctx.spawn(async move { oauth::refresh_access_token(&rt).await }, move |mm, res, ctx| {
        let w = mm.anthropic_refresh_waiters.take().unwrap_or_default();
        let o = match res {
            Ok(r) => { apply_anthropic_tokens(mm, r, ctx); AnthropicRefreshOutcome::Refreshed }
            Err(e) => { report_error!(e.context("Failed to refresh Anthropic OAuth token")); AnthropicRefreshOutcome::Failed }
        };
        for s in w { let _ = s.send(o); }
    });
}