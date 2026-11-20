//! Loopback redirect server for OAuth 2.0 authorization-code flows run from a native app
//! (RFC 8252): binds `127.0.0.1`, waits for the browser's single redirect to `/callback`, checks
//! the CSRF `state`, and returns the authorization code.

use std::future::Future;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, bail};
// `std::time::Instant` is disallowed (no wasm support); `instant::Instant` is a drop-in that
// re-exports the std type on native targets.
use instant::Instant;

const BIND_HOST: &str = "127.0.0.1";
/// How long to nap between non-blocking `accept()` and `read()` attempts.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long to wait for an accepted connection to send its request before giving up on it.
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-flow settings for a [`LoopbackListener`].
#[derive(Clone, Copy, Debug)]
pub struct LoopbackConfig {
    /// Product name used in error messages and the callback thread name, e.g. `"Grok"`.
    pub label: &'static str,
    /// Port to bind, or `0` for an OS-assigned port.
    pub port: u16,
    /// Host written into the redirect URI. The listener always binds `127.0.0.1`; use
    /// `"localhost"` for providers whose app registration only accepts that name.
    pub redirect_host: &'static str,
    /// How long to wait for the browser redirect before failing.
    pub timeout: Duration,
    /// Origins allowed to read the callback response cross-origin. Only needed when the
    /// provider's page fetches the callback instead of navigating to it.
    pub cors_allowed_origins: &'static [&'static str],
    pub success_html: &'static str,
    pub failure_html: &'static str,
}

/// A bound loopback callback server for one authorization attempt.
///
/// Bind before opening the browser so a bind failure surfaces before a browser tab opens.
pub struct LoopbackListener {
    listener: TcpListener,
    port: u16,
    config: LoopbackConfig,
}

/// Cancels an in-flight [`LoopbackListener::wait_for_code`].
#[derive(Clone, Debug, Default)]
pub struct CancellationHandle {
    cancelled: Arc<AtomicBool>,
}

impl CancellationHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Resolves once the listener's socket has been closed, strictly before the result future of
/// [`LoopbackListener::wait_for_code`] does any further work. Lets a caller learn the port is
/// free (to allow a retry) without waiting on work that follows the callback.
pub struct ReleaseSignal(async_channel::Receiver<()>);

impl ReleaseSignal {
    pub async fn released(self) {
        let _ = self.0.recv().await;
    }
}

/// The provider redirected back with an OAuth `error` instead of a code (for example, the user
/// declined consent or isn't assigned to the app).
#[derive(Debug, thiserror::Error)]
#[error("{label} authorization was denied or failed: {}", description.as_deref().unwrap_or(error))]
pub struct AuthorizationDenied {
    pub label: &'static str,
    /// The OAuth `error` code, e.g. `access_denied`.
    pub error: String,
    pub description: Option<String>,
}

impl LoopbackListener {
    pub fn bind(config: LoopbackConfig) -> anyhow::Result<Self> {
        let label = config.label;
        let listener = TcpListener::bind((BIND_HOST, config.port)).with_context(|| {
            format!(
                "couldn't bind the {label} sign-in callback server to {BIND_HOST}:{}. Another \
                 sign-in may be in progress, or another app is using the port.",
                config.port
            )
        })?;
        listener.set_nonblocking(true).with_context(|| {
            format!("failed to set the {label} callback listener to non-blocking mode")
        })?;
        let port = listener
            .local_addr()
            .with_context(|| format!("failed to read the {label} callback listener address"))?
            .port();
        Ok(Self {
            listener,
            port,
            config,
        })
    }

    /// The bound port; differs from [`LoopbackConfig::port`] when that was `0`.
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn redirect_uri(&self) -> String {
        format!(
            "http://{}:{}/callback",
            self.config.redirect_host, self.port
        )
    }

    /// Waits for the browser redirect, checks it against `expected_state`, and resolves to the
    /// authorization code. Fails with [`AuthorizationDenied`] when the provider reports an error.
    pub fn wait_for_code(
        self,
        expected_state: String,
        cancellation: CancellationHandle,
    ) -> (ReleaseSignal, impl Future<Output = anyhow::Result<String>>) {
        let (release_tx, release_rx) = async_channel::bounded(1);
        let result = run(self, expected_state, cancellation, release_tx);
        (ReleaseSignal(release_rx), result)
    }
}

async fn run(
    loopback: LoopbackListener,
    expected_state: String,
    cancellation: CancellationHandle,
    release_tx: async_channel::Sender<()>,
) -> anyhow::Result<String> {
    let LoopbackListener {
        listener, config, ..
    } = loopback;
    let label = config.label;
    // The accept loop is blocking, so run it on a dedicated OS thread and bridge the result back
    // through a runtime-agnostic async channel.
    let (tx, rx) = async_channel::bounded(1);
    let callback_thread = std::thread::Builder::new()
        .name(format!("{}-oauth-callback", label.to_lowercase()))
        .spawn(move || {
            let callback = wait_for_callback(&listener, &config, &cancellation);
            // Close the socket *before* publishing the result: whoever observes the result may
            // immediately rebind the port (a retried sign-in), which fails with "address in use"
            // while this listener is still open.
            drop(listener);
            // Signal release before publishing the callback: work after the callback doesn't
            // hold the port and shouldn't block a caller that only wants to retry.
            // `send_blocking` is disallowed (no wasm support); block this dedicated thread on the
            // async `send` instead.
            let _ = futures::executor::block_on(release_tx.send(()));
            let _ = futures::executor::block_on(tx.send(callback));
        })
        .with_context(|| format!("failed to spawn the {label} callback server thread"))?;

    let callback = rx
        .recv()
        .await
        .with_context(|| format!("the {label} callback server stopped unexpectedly"));

    // Publishing the result is the callback thread's last act, so this join returns
    // immediately; it guarantees the thread and every resource it owned are torn down before
    // control returns, on the cancelled, timed-out, and completed paths alike.
    let _ = callback_thread.join();

    let callback = callback??;
    if callback.state != expected_state {
        bail!("the authorization response state did not match — aborting to prevent CSRF");
    }
    Ok(callback.code)
}

struct CallbackData {
    code: String,
    state: String,
}

fn wait_for_callback(
    listener: &TcpListener,
    config: &LoopbackConfig,
    cancellation: &CancellationHandle,
) -> anyhow::Result<CallbackData> {
    let label = config.label;
    let deadline = Instant::now() + config.timeout;
    loop {
        if cancellation.is_cancelled() {
            bail!("{label} authorization was cancelled");
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for the {label} authorization callback");
        }
        match listener.accept() {
            Ok((stream, _)) => match handle_callback_connection(stream, config, cancellation)? {
                Some(data) => return Ok(data),
                // Unrelated request (e.g. /favicon.ico); keep waiting.
                None => continue,
            },
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => {
                return Err(
                    anyhow::Error::new(e).context(format!("{label} callback accept failed"))
                );
            }
        }
    }
}

/// Reads one HTTP request, writes a minimal HTML response, and extracts the OAuth parameters.
///
/// Returns `Ok(None)` for requests that aren't the OAuth callback (so the caller keeps
/// listening), `Ok(Some(..))` on a successful callback, and `Err` when the provider reported an
/// error or the callback was malformed.
fn handle_callback_connection(
    mut stream: TcpStream,
    config: &LoopbackConfig,
    cancellation: &CancellationHandle,
) -> anyhow::Result<Option<CallbackData>> {
    let label = config.label;
    let request = read_callback_request(&mut stream, label, cancellation)?;
    let origin = request_header(&request, "Origin");
    let respond = |stream: &mut TcpStream, status: &str, body: &str| {
        write_response(stream, status, body, origin.as_deref(), config);
    };

    // The request line looks like: "GET /callback?code=...&state=... HTTP/1.1".
    let mut request_line_parts = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    let method = request_line_parts.next().unwrap_or_default();
    let path = request_line_parts.next().unwrap_or_default();

    if method == "OPTIONS" && path.starts_with("/callback") {
        respond(&mut stream, "204 No Content", "");
        return Ok(None);
    }

    let Some(query) = path
        .strip_prefix("/callback")
        .and_then(|rest| rest.strip_prefix('?'))
    else {
        respond(&mut stream, "404 Not Found", "Not found.");
        return Ok(None);
    };

    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut error_description = None;
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(query).unwrap_or_default();
    for (key, value) in pairs {
        match key.as_str() {
            "code" => code = Some(value),
            "state" => state = Some(value),
            "error" => error = Some(value),
            "error_description" => error_description = Some(value),
            _ => {}
        }
    }

    if let Some(error) = error {
        respond(&mut stream, "400 Bad Request", config.failure_html);
        return Err(AuthorizationDenied {
            label,
            error,
            description: error_description,
        }
        .into());
    }

    let (Some(code), Some(state)) = (code, state) else {
        respond(&mut stream, "400 Bad Request", config.failure_html);
        bail!("the {label} authorization callback was missing the code or state parameter");
    };
    respond(&mut stream, "200 OK", config.success_html);
    Ok(Some(CallbackData { code, state }))
}

/// Reads `stream` until its headers are fully received (a blank line), the buffer fills, or the
/// connection closes, polling in short increments so a cancellation is noticed promptly rather
/// than only once `CALLBACK_READ_TIMEOUT` elapses. Otherwise a connection that's accepted but
/// never sends anything (e.g. a stray network probe) would hold the listener for that long after
/// cancelling.
fn read_callback_request(
    stream: &mut TcpStream,
    label: &str,
    cancellation: &CancellationHandle,
) -> anyhow::Result<String> {
    stream
        .set_nonblocking(true)
        .with_context(|| format!("failed to set the {label} callback stream to non-blocking"))?;
    let deadline = Instant::now() + CALLBACK_READ_TIMEOUT;
    let mut buf = [0u8; 8192];
    let mut total = 0;
    loop {
        if cancellation.is_cancelled() {
            bail!("{label} authorization was cancelled");
        }
        // Checked every iteration, not just after a `WouldBlock`, so a client that drips one
        // byte per poll interval still can't hold the connection past `CALLBACK_READ_TIMEOUT`.
        if Instant::now() >= deadline {
            bail!("timed out reading the {label} callback request");
        }
        match stream.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                if total == buf.len() || buf[..total].windows(4).any(|window| window == b"\r\n\r\n")
                {
                    break;
                }
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => {
                return Err(anyhow::Error::new(e)
                    .context(format!("failed to read the {label} callback request")));
            }
        }
    }
    Ok(String::from_utf8_lossy(&buf[..total]).into_owned())
}

fn request_header(request: &str, header_name: &str) -> Option<String> {
    request.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(header_name)
            .then(|| value.trim().to_owned())
    })
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    body: &str,
    origin: Option<&str>,
    config: &LoopbackConfig,
) {
    let cors_headers = cors_headers(origin, config.cors_allowed_origins);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         {cors_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}

/// Cross-origin callback fetches (`https://provider` -> `http://127.0.0.1`) need CORS and Private
/// Network Access headers before the provider's page can observe the response.
fn cors_headers(origin: Option<&str>, allowed_origins: &[&str]) -> String {
    origin
        .filter(|origin| allowed_origins.contains(origin))
        .map(|origin| {
            format!(
                "Access-Control-Allow-Origin: {origin}\r\n\
                 Access-Control-Allow-Methods: GET, OPTIONS\r\n\
                 Access-Control-Allow-Headers: Content-Type\r\n\
                 Access-Control-Allow-Private-Network: true\r\n\
                 Vary: Origin\r\n"
            )
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
