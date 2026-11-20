use std::net::SocketAddr;
use std::thread::JoinHandle;

use super::*;

const TEST_CONFIG: LoopbackConfig = LoopbackConfig {
    label: "Test",
    port: 0,
    redirect_host: "localhost",
    timeout: Duration::from_secs(30),
    cors_allowed_origins: &["https://provider.example"],
    success_html: "ok",
    failure_html: "failed",
};

/// Ports below every platform's ephemeral range, so a concurrent test's ephemeral bind can't
/// collide with the exact-port rebinds asserted below.
const TEST_PORT_BASE: u16 = 22121;
const TEST_PORT_SPAN: u16 = 200;

fn bind_test_listener() -> (LoopbackListener, SocketAddr) {
    // Scanning from a process-dependent offset keeps two test binaries off each other's ports.
    let offset = (std::process::id() % u32::from(TEST_PORT_SPAN)) as u16;
    for candidate in 0..TEST_PORT_SPAN {
        let port = TEST_PORT_BASE + (offset + candidate) % TEST_PORT_SPAN;
        let Ok(listener) = LoopbackListener::bind(LoopbackConfig {
            port,
            ..TEST_CONFIG
        }) else {
            continue;
        };
        let address = SocketAddr::from(([127, 0, 0, 1], listener.port()));
        return (listener, address);
    }
    panic!(
        "no free loopback port in {TEST_PORT_BASE}..{} for the test callback listener",
        TEST_PORT_BASE + TEST_PORT_SPAN
    );
}

/// Sends `request_line` as a browser would and returns the thread reading the response.
fn send_browser_request(address: SocketAddr, request_line: &str) -> JoinHandle<String> {
    let request = format!("{request_line}\r\nHost: 127.0.0.1\r\n\r\n");
    std::thread::spawn(move || {
        let mut stream = TcpStream::connect(address).expect("test browser should connect");
        stream
            .write_all(request.as_bytes())
            .expect("test browser should send the callback request");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("test browser should read the callback response");
        response
    })
}

fn wait(listener: LoopbackListener, state: &str) -> anyhow::Result<String> {
    let (_release, result) = listener.wait_for_code(state.to_owned(), CancellationHandle::new());
    futures::executor::block_on(result)
}

#[test]
fn os_assigned_port_is_reported_in_the_redirect_uri() {
    let listener = LoopbackListener::bind(TEST_CONFIG).expect("should bind an ephemeral port");
    assert_ne!(listener.port(), 0);
    assert_eq!(
        listener.redirect_uri(),
        format!("http://localhost:{}/callback", listener.port())
    );
}

#[test]
#[serial_test::serial(oauth_loopback_port)]
fn matching_callback_returns_the_code() {
    let (listener, address) = bind_test_listener();
    let browser = send_browser_request(address, "GET /callback?code=the-code&state=s1 HTTP/1.1");
    let code = wait(listener, "s1").expect("a matching callback should succeed");
    assert_eq!(code, "the-code");
    assert!(browser.join().unwrap().ends_with("ok"));
}

#[test]
#[serial_test::serial(oauth_loopback_port)]
fn unrelated_requests_are_ignored_until_the_callback_arrives() {
    let (listener, address) = bind_test_listener();
    let favicon = send_browser_request(address, "GET /favicon.ico HTTP/1.1");
    let result = std::thread::spawn(move || wait(listener, "s1"));
    assert!(favicon.join().unwrap().starts_with("HTTP/1.1 404"));
    let browser = send_browser_request(address, "GET /callback?code=c&state=s1 HTTP/1.1");
    assert_eq!(result.join().unwrap().unwrap(), "c");
    browser.join().unwrap();
}

#[test]
#[serial_test::serial(oauth_loopback_port)]
fn provider_errors_surface_as_authorization_denied() {
    let (listener, address) = bind_test_listener();
    let browser = send_browser_request(
        address,
        "GET /callback?error=access_denied&error_description=not%20assigned HTTP/1.1",
    );
    let error = wait(listener, "s1").expect_err("a provider error should fail");
    let denied = error
        .downcast_ref::<AuthorizationDenied>()
        .expect("should be an AuthorizationDenied error");
    assert_eq!(denied.error, "access_denied");
    assert_eq!(
        error.to_string(),
        "Test authorization was denied or failed: not assigned"
    );
    assert!(browser.join().unwrap().ends_with("failed"));
}

#[test]
#[serial_test::serial(oauth_loopback_port)]
fn cors_headers_are_sent_only_to_allowed_origins() {
    assert!(
        cors_headers(
            Some("https://provider.example"),
            TEST_CONFIG.cors_allowed_origins
        )
        .contains("Access-Control-Allow-Origin: https://provider.example")
    );
    assert!(
        cors_headers(
            Some("https://evil.example"),
            TEST_CONFIG.cors_allowed_origins
        )
        .is_empty()
    );
    assert!(cors_headers(None, TEST_CONFIG.cors_allowed_origins).is_empty());
}

/// The result must not be handed back while the callback thread still owns the socket, or an
/// immediate rebind fails with "address in use".
#[test]
#[serial_test::serial(oauth_loopback_port)]
fn cancelling_the_wait_releases_the_listener() {
    const CANCEL_CYCLES: usize = 100;

    for _ in 0..CANCEL_CYCLES {
        let (listener, address) = bind_test_listener();
        let cancellation = CancellationHandle::new();
        cancellation.cancel();

        let (_release, result) = listener.wait_for_code("s1".to_owned(), cancellation);
        let result = futures::executor::block_on(result);

        assert_eq!(
            result
                .expect_err("cancelled callback wait should fail")
                .to_string(),
            "Test authorization was cancelled"
        );
        TcpListener::bind(address).expect("cancelled callback listener should release its port");
    }
}

/// A caller only needs the release signal, not the full result, to know the port is free again,
/// proven here by rebinding without ever joining the result thread.
#[test]
#[serial_test::serial(oauth_loopback_port)]
fn release_signal_lets_a_caller_rebind_without_joining_the_result() {
    let (listener, address) = bind_test_listener();
    let browser = send_browser_request(
        address,
        "GET /callback?code=test-code&state=unexpected-state HTTP/1.1",
    );

    let (release, result) = listener.wait_for_code("s1".to_owned(), CancellationHandle::new());
    let result_thread = std::thread::spawn(move || futures::executor::block_on(result));

    futures::executor::block_on(release.released());
    TcpListener::bind(address).expect("released listener should free its port immediately");

    let result = result_thread.join().expect("result future should finish");
    browser.join().expect("test browser thread should finish");
    assert!(
        result
            .expect_err("a mismatched callback state should fail")
            .to_string()
            .contains("state did not match")
    );
}

/// Waits for `listener` (non-blocking) to accept a connection that was just established.
/// Bounded retry is about OS handshake scheduling, not the behavior under test below.
fn accept_test_connection(listener: &TcpListener) -> TcpStream {
    for _ in 0..100 {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => panic!("test listener failed to accept a connection: {e}"),
        }
    }
    panic!("test listener never accepted the connection");
}

/// A connection that's accepted but never sends a request must not hold the read past a poll
/// interval once cancelled, even though `CALLBACK_READ_TIMEOUT` is far longer.
///
/// The reader signals once it has started, and cancellation is then held off for a couple of
/// poll intervals so it lands while the reader is asleep between polls rather than racing its
/// first entry check; there is no externally observable "completed one poll" signal, so the
/// margin is sized off `POLL_INTERVAL` itself.
#[test]
#[serial_test::serial(oauth_loopback_port)]
fn cancelling_with_a_stalled_accepted_connection_still_releases_promptly() {
    let (listener, address) = bind_test_listener();
    let cancellation = CancellationHandle::new();

    // Connect without ever sending data, standing in for a stray probe or a browser tab that
    // opened the socket but hasn't sent the request yet.
    let stalled_client = TcpStream::connect(address).expect("stalled client should connect");
    let mut accepted = accept_test_connection(&listener.listener);

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let reader_cancellation = cancellation.clone();
    let reader = std::thread::spawn(move || {
        let _ = started_tx.send(());
        read_callback_request(&mut accepted, "Test", &reader_cancellation)
    });
    started_rx
        .recv()
        .expect("reader thread should signal that it has started");
    std::thread::sleep(POLL_INTERVAL * 2);

    cancellation.cancel();

    let started_waiting = Instant::now();
    let result = reader.join().expect("reader thread should finish");
    let elapsed = started_waiting.elapsed();

    assert_eq!(
        result
            .expect_err("a cancelled read should fail")
            .to_string(),
        "Test authorization was cancelled"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "reading a stalled, cancelled connection took {elapsed:?}; expected it to return \
         immediately rather than blocking for CALLBACK_READ_TIMEOUT"
    );
    drop(stalled_client);
}
