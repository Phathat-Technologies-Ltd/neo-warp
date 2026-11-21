pub mod ant_cli;
pub mod oauth;

use std::time::{Duration, SystemTime};

use futures::channel::oneshot;
use warp_errors::report_error;
use warpui_core::ModelContext;
use warpui_core::r#async::Timer;

use self::oauth::TokenResponse;
use crate::api_keys::{
    AnthropicRefreshOutcome, AnthropicTokenSource, AnthropicTokens, ApiKeyManager,
};

const REFRESH_LEAD_TIME: Duration = Duration::from_secs(5 * 60);

pub fn anthropic_tokens_from_response(
    response: TokenResponse,
    previous: Option<&AnthropicTokens>,
    source: AnthropicTokenSource,
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
        source,
    }
}

/// How an expiring token gets replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Refresh {
    /// Trade this refresh token at Anthropic's token endpoint.
    RefreshToken(String),
    /// Ask the `ant` CLI, which holds and refreshes the credentials itself.
    AntCli,
}

impl AnthropicTokens {
    fn refresh(&self) -> Option<Refresh> {
        match self.source {
            AnthropicTokenSource::OAuth => self.refresh_token.clone().map(Refresh::RefreshToken),
            AnthropicTokenSource::AntCli => Some(Refresh::AntCli),
        }
    }
}

impl ApiKeyManager {
    pub fn store_anthropic_tokens(&mut self, r: TokenResponse, ctx: &mut ModelContext<Self>) {
        apply_anthropic_tokens(self, r, AnthropicTokenSource::OAuth, ctx);
    }

    /// Stores a token from the `ant` CLI's sign-in; later refreshes go through `ant` too.
    pub fn store_ant_cli_tokens(&mut self, r: TokenResponse, ctx: &mut ModelContext<Self>) {
        apply_anthropic_tokens(self, r, AnthropicTokenSource::AntCli, ctx);
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

    fn anthropic_expired_refresh(&self, byo_allowed: bool) -> Option<Refresh> {
        if !byo_allowed {
            return None;
        }
        let t = self.anthropic_tokens()?;
        if !t.is_expired() {
            return None;
        }
        t.refresh()
    }

    pub fn begin_expired_anthropic_refresh(
        &mut self,
        byo_allowed: bool,
        ctx: &mut ModelContext<Self>,
    ) -> Option<oneshot::Receiver<AnthropicRefreshOutcome>> {
        self.set_anthropic_refresh_allowed(byo_allowed, ctx);
        let refresh = self.anthropic_expired_refresh(byo_allowed)?;
        let (tx, rx) = oneshot::channel();
        spawn_anthropic_refresh(self, refresh, vec![tx], ctx);
        Some(rx)
    }
}

fn apply_anthropic_tokens(
    m: &mut ApiKeyManager,
    r: TokenResponse,
    source: AnthropicTokenSource,
    ctx: &mut ModelContext<ApiKeyManager>,
) {
    let t = anthropic_tokens_from_response(r, m.anthropic_tokens(), source);
    m.set_anthropic_tokens(Some(t), ctx);
    schedule_anthropic_refresh(m, ctx);
}

fn schedule_anthropic_refresh(m: &mut ApiKeyManager, ctx: &mut ModelContext<ApiKeyManager>) {
    if !m.anthropic_refresh_allowed {
        return;
    }
    let Some(t) = m.anthropic_tokens().cloned() else {
        return;
    };
    let Some(refresh) = t.refresh() else {
        return;
    };
    let Some(exp) = t.expires_at else {
        return;
    };
    let now = SystemTime::now();
    let fire = exp.checked_sub(REFRESH_LEAD_TIME).unwrap_or(now);
    let delay = fire.duration_since(now).unwrap_or(Duration::ZERO);
    let access_token = t.access_token;
    ctx.spawn(
        async move {
            Timer::after(delay).await;
        },
        move |mm, _, ctx| {
            if !mm.anthropic_refresh_allowed {
                return;
            }
            // Skip a refresh scheduled for tokens that were replaced or disconnected since.
            let current = mm
                .anthropic_tokens()
                .is_some_and(|t| t.access_token == access_token);
            if current {
                spawn_anthropic_refresh(mm, refresh, Vec::new(), ctx);
            }
        },
    );
}

fn spawn_anthropic_refresh(
    m: &mut ApiKeyManager,
    refresh: Refresh,
    waiters: Vec<oneshot::Sender<AnthropicRefreshOutcome>>,
    ctx: &mut ModelContext<ApiKeyManager>,
) {
    if let Some(e) = m.anthropic_refresh_waiters.as_mut() {
        e.extend(waiters);
        return;
    }
    m.anthropic_refresh_waiters = Some(waiters);
    let source = match refresh {
        Refresh::RefreshToken(_) => AnthropicTokenSource::OAuth,
        Refresh::AntCli => AnthropicTokenSource::AntCli,
    };
    ctx.spawn(
        async move {
            match refresh {
                Refresh::RefreshToken(rt) => oauth::refresh_access_token(&rt).await,
                Refresh::AntCli => match ant_cli::get() {
                    Some(ant) => ant.token().await,
                    None => Err(anyhow::anyhow!(
                        "the Claude sign-in came from the ant CLI, which isn't available"
                    )),
                },
            }
        },
        move |mm, res, ctx| {
            let w = mm.anthropic_refresh_waiters.take().unwrap_or_default();
            let o = match res {
                Ok(r) => {
                    apply_anthropic_tokens(mm, r, source, ctx);
                    AnthropicRefreshOutcome::Refreshed
                }
                Err(e) => {
                    report_error!(e.context("Failed to refresh Anthropic OAuth token"));
                    AnthropicRefreshOutcome::Failed
                }
            };
            for s in w {
                let _ = s.send(o);
            }
        },
    );
}
