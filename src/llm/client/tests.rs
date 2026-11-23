//! Client tests against a scripted server: one canned raw response per
//! connection, connections counted — the attempt count asserted rather
//! than assumed. `Connection: close` on every response keeps connections
//! and responses in step, retries included.

use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// A scripted server that answers one raw response per connection and
/// counts the connections it accepted — the attempt count, asserted
/// rather than assumed. `Connection: close` on every response keeps
/// connections and responses in step, retries included. Shared with
/// [`super::stream_tests`], which scripts the same harness against SSE.
pub(super) struct Scripted {
    pub(super) base_url: String,
    pub(super) attempts: Arc<AtomicUsize>,
}

impl Scripted {
    pub(super) fn spawn(responses: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let counted = attempts.clone();
        std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                counted.fetch_add(1, Ordering::SeqCst);
                read_request_head(&mut stream);
                stream.write_all(response.as_bytes()).unwrap();
                drop(stream);
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            attempts,
        }
    }

    /// A client that decides exactly like production but never sleeps,
    /// so retry tests stay instant.
    pub(super) fn client(&self) -> LlmClient {
        LlmClient::new(&self.base_url)
            .unwrap()
            .with_retry_delay(Duration::ZERO)
    }
}

pub(super) fn read_request_head(stream: &mut std::net::TcpStream) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "connection closed before headers finished");
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// The flag for a call nobody stopped. `send_stream` takes the run's stop
/// request, so every script that is not about aborting hands it a flag
/// that stays clear.
pub(super) fn never_abort() -> AtomicBool {
    AtomicBool::new(false)
}

pub(super) fn http(status: &str, extra_headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub(super) fn ok_body(text: &str) -> String {
    format!(
        r##"{{"choices":[{{"message":{{"role":"assistant","content":"{text}"}},"finish_reason":"stop"}}]}}"##
    )
}

pub(super) fn sse(text: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\ndata: [DONE]\n\n"
    )
}

#[test]
fn opencode_url_detection() {
    assert!(is_opencode_url(
        "https://opencode.ai/inference/go/openai/v1"
    ));
    assert!(is_opencode_url(
        "https://opencode.ai/inference/go/anthropic"
    ));
    assert!(!is_opencode_url("https://api.deepseek.com"));
    assert!(!is_opencode_url("http://127.0.0.1:8080/v1"));
    assert!(!is_opencode_url("https://notopencode.ai/v1"));
}

#[tokio::test]
async fn session_header_is_sent_when_configured() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(String::new()));
    let seen_clone = seen.clone();

    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let n = s.read(&mut buf).unwrap();
        *seen_clone.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        s.write_all(resp.as_bytes()).unwrap();
    });

    let client = LlmClient::new(&format!("http://{addr}"))
        .unwrap()
        .with_session_header(Some("x-opencode-session".into()))
        .with_session_id("sess-123".into());
    let _ = client.send("m", &[], 16, &[]).await.unwrap();
    let raw = seen.lock().unwrap().to_ascii_lowercase();
    assert!(raw.contains("x-opencode-session: sess-123"), "{raw}");
    assert!(raw.contains("x-opencode-client: ganggang"), "{raw}");
    assert!(raw.contains("user-agent: ganggang/"), "{raw}");
}

/// A provider having a bad minute (500) is retried, and the retry
/// succeeds: two connections, one answer.
#[tokio::test]
async fn a_500_is_retried_and_the_retry_succeeds() {
    let server = Scripted::spawn(vec![
        http("500 Internal Server Error", "", "upstream exploded"),
        http("200 OK", "", &ok_body("gg")),
    ]);
    let resp = server.client().send("m", &[], 16, &[]).await.unwrap();
    assert_eq!(resp.choices[0].message.content.as_deref(), Some("gg"));
    assert_eq!(server.attempts.load(Ordering::SeqCst), 2);
}

/// A 400 is the request's own fault: exactly one attempt, and the
/// provider's body is what the caller reads.
#[tokio::test]
async fn a_400_is_not_retried() {
    let server = Scripted::spawn(vec![http("400 Bad Request", "", "bad model name")]);
    let err = server.client().send("m", &[], 16, &[]).await.unwrap_err();
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("400") && rendered.contains("bad model name"),
        "{rendered}"
    );
    assert_eq!(server.attempts.load(Ordering::SeqCst), 1);
}

/// 429 with a `Retry-After` is retried; the header (0 s here) decides
/// the wait instead of the doubling base.
#[tokio::test]
async fn a_429_with_retry_after_is_retried() {
    let server = Scripted::spawn(vec![
        http(
            "429 Too Many Requests",
            "Retry-After: 0\r\n",
            "rate limit exceeded",
        ),
        http("200 OK", "", &ok_body("later")),
    ]);
    let resp = server.client().send("m", &[], 16, &[]).await.unwrap();
    assert_eq!(resp.choices[0].message.content.as_deref(), Some("later"));
    assert_eq!(server.attempts.load(Ordering::SeqCst), 2);
}

/// The budget is bounded: retries stop at `max_retries`, and the error
/// the caller gets is the last attempt's — status and body included.
#[tokio::test]
async fn retries_stop_at_the_budget_and_keep_the_last_error() {
    let server = Scripted::spawn(vec![
        http("503 Service Unavailable", "", "one"),
        http("503 Service Unavailable", "", "last"),
    ]);
    let client = server.client().with_retry(1);
    let err = client.send("m", &[], 16, &[]).await.unwrap_err();
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("503") && rendered.contains("last"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("one"),
        "the first failure leaked in: {rendered}"
    );
    assert_eq!(server.attempts.load(Ordering::SeqCst), 2);
}

/// The whole-response timeout is a setting, carried on the request
/// (where reqwest applies it to head AND body) rather than baked into
/// the client at 300 s.
#[test]
fn the_request_timeout_is_configurable_and_carried_on_the_request() {
    let client = LlmClient::new("http://127.0.0.1:1").unwrap();
    assert_eq!(client.request_timeout_secs(), 600, "the default");
    let client = client.with_request_timeout(900);
    assert_eq!(client.request_timeout_secs(), 900);
    let (url, request) =
        build_request(client.http.get(&client.base_url), client.request_timeout).unwrap();
    assert_eq!(request.timeout(), Some(&Duration::from_secs(900)));
    assert!(url.starts_with("http://127.0.0.1:1"));
    // A replayed attempt keeps the same timeout.
    assert_eq!(
        replay(&request).unwrap().timeout(),
        Some(&Duration::from_secs(900))
    );
    // Zero is not a usable deadline; the floor keeps a hang bounded.
    assert_eq!(client.with_request_timeout(0).request_timeout_secs(), 1);
}
