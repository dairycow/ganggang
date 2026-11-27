//! Client tests against a scripted server: one canned raw response per
//! connection, connections counted (the attempt count asserted rather than
//! assumed) and each connection's raw request head recorded for the tests
//! that read the wire. `Connection: close` on every response keeps
//! connections and responses in step, retries included.

use super::attempt::replay;
use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// A scripted server that answers one raw response per connection, counts
/// the connections it accepted (the attempt count, asserted rather than
/// assumed) and records each connection's raw request head. `Connection:
/// close` on every response keeps connections and responses in step,
/// retries included. Shared with [`super::stream_tests`], which scripts
/// the same harness against SSE.
pub(super) struct Scripted {
    pub(super) base_url: String,
    pub(super) attempts: Arc<AtomicUsize>,
    /// Each connection's raw request head, in the order they arrived.
    heads: Arc<Mutex<Vec<String>>>,
}

impl Scripted {
    pub(super) fn spawn(responses: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let counted = attempts.clone();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let recorded = heads.clone();
        std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                counted.fetch_add(1, Ordering::SeqCst);
                let head = read_request_head(&mut stream);
                recorded.lock().unwrap().push(head);
                stream.write_all(response.as_bytes()).unwrap();
                drop(stream);
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            attempts,
            heads,
        }
    }

    /// A client that decides exactly like production but never sleeps,
    /// so retry tests stay instant.
    pub(super) fn client(&self) -> LlmClient {
        LlmClient::new(&self.base_url)
            .unwrap()
            .with_retry_delay(Duration::ZERO)
    }

    /// A client built the way production builds one, for `endpoint`, then
    /// re-pointed at this server. What the endpoint's own URL settled when
    /// the client was built (the attribution, the session header) stands
    /// after the re-point: only the address changes.
    pub(super) fn client_for(&self, endpoint: &crate::config::Endpoint) -> LlmClient {
        LlmClient::from_endpoint(endpoint)
            .unwrap()
            .with_base_url(self.base_url.clone())
    }

    /// The raw request head of connection `nth`, counting from zero.
    pub(super) fn head(&self, nth: usize) -> String {
        let heads = self.heads.lock().unwrap();
        heads.get(nth).cloned().unwrap_or_else(|| {
            panic!(
                "connection {nth} was never made: {} head(s) recorded",
                heads.len()
            )
        })
    }
}

/// Read up to the end of the request head and return what was read: the
/// head, plus any of the body that arrived with it. A test reading the wire
/// looks for whole head lines, which either form carries.
pub(super) fn read_request_head(stream: &mut std::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "connection closed before headers finished");
        buf.extend_from_slice(&chunk[..n]);
    }
    String::from_utf8_lossy(&buf).to_string()
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

/// Build a client for `endpoint` the way production does, point it at a
/// loopback server bound here, make one non-streaming send, and return the
/// raw request head. Nothing leaves the loopback: the only address the
/// client is ever told to call is the server this function made.
pub(super) async fn capture_head(endpoint: &crate::config::Endpoint) -> String {
    let server = Scripted::spawn(vec![http("200 OK", "", &ok_body("gg"))]);
    let result = server.client_for(endpoint).send("m", &[], 16, &[]).await;
    let raw = server.head(0);
    if let Err(err) = result {
        panic!("the request failed, so the headers could not be read: {err:#}\nraw head: {raw}");
    }
    raw
}

/// Assert both attribution lines as they stand on the wire, name and value
/// each on a line of its own: a dropped header fails this, and so does a
/// changed value.
pub(super) fn assert_openrouter_attribution(raw: &str) {
    assert!(
        raw.contains("\r\nhttp-referer: https://ganggang.io\r\n"),
        "no referer to ganggang.io: {raw}"
    );
    assert!(raw.contains("\r\nx-title: ganggang\r\n"), "no title: {raw}");
}

/// The attribution headers reach the wire on the non-streaming path, with
/// the values OpenRouter credits the app by: its site and its name.
#[tokio::test]
async fn openrouter_attribution_headers_are_sent() {
    let raw = capture_head(&openrouter_endpoint()).await;
    assert_openrouter_attribution(&raw);
    assert!(raw.contains("user-agent: ganggang/"), "{raw}");
}

/// An OpenRouter endpoint is attributed to the app's own site and name, with
/// neither `GG_OPENROUTER_*` variable exported, and any other endpoint gets
/// no attribution at all: the gate the wire tests read from the other side.
#[test]
fn openrouter_attribution_defaults_to_the_app_and_stays_off_elsewhere() {
    let attribution = OpenRouterAttribution::from_base_url("https://openrouter.ai/api/v1")
        .expect("an OpenRouter endpoint is attributed");
    assert_eq!(attribution.referer, "https://ganggang.io");
    assert_eq!(attribution.title, "ganggang");
    assert!(
        OpenRouterAttribution::from_base_url("https://api.deepseek.com").is_none(),
        "another provider's endpoint is not attributed"
    );
}

/// The host gate is the same check production runs, and it is what keeps the
/// headers off every other OpenAI-compatible endpoint.
#[test]
fn openrouter_attribution_greys_to_openrouter_hosts_only() {
    assert!(is_openrouter_url("https://openrouter.ai/api/v1"));
    assert!(!is_openrouter_url("http://127.0.0.1:1/v1"));
    assert!(!is_openrouter_url("https://api.deepseek.com"));
    assert!(!is_openrouter_url(
        "https://opencode.ai/inference/go/openai/v1"
    ));
    assert!(!is_openrouter_url("https://notopenrouter.ai/v1"));
}

/// A client built for a local endpoint sends no attribution, even though it is
/// the same client type: the headers come from the host check alone. Built the
/// same way production builds one, only with a deepseek URL.
#[tokio::test]
async fn openrouter_attribution_is_omitted_for_other_hosts() {
    let raw = capture_head(&deepseek_endpoint()).await;
    assert!(!raw.contains("http-referer"), "{raw}");
    assert!(!raw.contains("x-title"), "{raw}");
}

/// A minimally-built OpenRouter endpoint: enough for `from_endpoint` and one
/// send. Its URL is never contacted: `Scripted::client_for` re-points the
/// client at the loopback test server.
pub(super) fn openrouter_endpoint() -> crate::config::Endpoint {
    crate::config::Endpoint {
        provider: "openrouter".to_string(),
        model: "upstage/solar-mini-4-5".to_string(),
        model_explicit: true,
        base_url: "https://openrouter.ai/api/v1".to_string(),
        api: super::ApiKind::OpenAiCompletions,
        auth: super::AuthStyle::Auto,
        api_key: Some("test-key".to_string()),
        api_key_env: None,
        max_tokens: 16,
        context_window: 131_072,
        context_window_explicit: true,
        replay_reasoning: crate::agent::session::ReplayReasoning::Never,
        reasoning_effort: None,
        reasoning_budget_tokens: None,
        compact_threshold_tokens: None,
        compact_threshold_explicit: true,
    }
}

/// A minimally-built deepseek endpoint, the same shape with a non-OpenRouter
/// host so the gate has something to reject.
fn deepseek_endpoint() -> crate::config::Endpoint {
    let mut endpoint = openrouter_endpoint();
    endpoint.provider = "deepseek".to_string();
    endpoint.base_url = "https://api.deepseek.com".to_string();
    endpoint.model = "deepseek-flash".to_string();
    endpoint
}

/// A minimally-built opencode endpoint, so the client is built with the
/// session header opencode expects. Never contacted either.
fn opencode_endpoint() -> crate::config::Endpoint {
    let mut endpoint = openrouter_endpoint();
    endpoint.provider = "opencode".to_string();
    endpoint.base_url = "https://opencode.ai/inference/go/openai/v1".to_string();
    endpoint
}

/// An opencode endpoint sends the session headers opencode reads: the run's
/// session id, and the client's name beside it. Both are settled from the
/// endpoint's URL when the client is built, like the attribution.
#[tokio::test]
async fn opencode_session_headers_are_sent() {
    let server = Scripted::spawn(vec![http("200 OK", "", &ok_body("gg"))]);
    server
        .client_for(&opencode_endpoint())
        .with_session_id("sess-123".to_string())
        .send("m", &[], 16, &[])
        .await
        .unwrap();
    let raw = server.head(0);
    assert!(
        raw.contains("\r\nx-opencode-session: sess-123\r\n"),
        "no session id: {raw}"
    );
    assert!(
        raw.contains("\r\nx-opencode-client: ganggang\r\n"),
        "no client name: {raw}"
    );
}

/// The same attribution on a non-streaming send that takes the Anthropic
/// path: the client adds it before the API key whichever API the endpoint
/// speaks, so an OpenRouter endpoint configured for Anthropic Messages is
/// credited like a chat-completions one.
#[tokio::test]
async fn openrouter_attribution_headers_are_sent_on_an_anthropic_send() {
    const ANSWER: &str = r#"{"content":[{"type":"text","text":"ok"}]}"#;
    let server = Scripted::spawn(vec![http("200 OK", "", ANSWER)]);
    let mut endpoint = openrouter_endpoint();
    endpoint.api = ApiKind::AnthropicMessages;
    let response = server
        .client_for(&endpoint)
        .send("m", &[], 16, &[])
        .await
        .unwrap();
    assert_eq!(
        response.choices[0].message.content.as_deref(),
        Some("ok"),
        "the Anthropic path answered"
    );
    let raw = server.head(0);
    assert!(raw.starts_with("POST /v1/messages "), "{raw}");
    assert_openrouter_attribution(&raw);
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
    assert!(
        is_refusal(&err),
        "the server answered the request itself: {rendered}"
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
        !is_refusal(&err),
        "a 503 is the server failing, not refusing the request: {rendered}"
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

/// A server that answers one request with `body` and hands the raw
/// request back for inspection.
fn echo_request(body: &'static str) -> (String, Arc<Mutex<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(String::new()));
    let sink = seen.clone();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let n = s.read(&mut buf).unwrap();
        *sink.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        s.write_all(resp.as_bytes()).unwrap();
    });
    (format!("http://{addr}"), seen)
}

const OK: &str =
    r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#;

#[tokio::test]
async fn reasoning_effort_and_budget_are_sent_when_configured() {
    let (base_url, seen) = echo_request(OK);
    LlmClient::new(&base_url)
        .unwrap()
        .with_reasoning_effort(Some("medium".into()))
        .with_reasoning_budget_tokens(Some(4096))
        .send("m", &[], 16, &[])
        .await
        .unwrap();
    let raw = seen.lock().unwrap().clone();
    let body = raw.split("\r\n\r\n").nth(1).unwrap();
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["reasoning_effort"], "medium");
    assert_eq!(v["reasoning_budget_tokens"], 4096);
}

/// A request without reasoning carries `reasoning_effort: "none"` and no
/// budget whatever the endpoint configured, and changes nothing the next
/// ordinary request sends. [`LlmClient::without_reasoning_field`] names
/// that field the way the wire spells it.
#[tokio::test]
async fn a_request_without_reasoning_sends_none_and_no_budget() {
    let (base_url, seen) = echo_request(OK);
    let client = LlmClient::new(&base_url)
        .unwrap()
        .with_reasoning_effort(Some("medium".into()))
        .with_reasoning_budget_tokens(Some(4096));
    client
        .send_without_reasoning("m", &[], 16, &[])
        .await
        .unwrap();
    let raw = seen.lock().unwrap().clone();
    let body = raw.split("\r\n\r\n").nth(1).unwrap();
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["reasoning_effort"], "none");
    assert!(v.get("reasoning_budget_tokens").is_none(), "{v}");
    assert_eq!(
        client.without_reasoning_field(),
        Some("reasoning_effort: \"none\""),
        "the warning's words are the request's own"
    );
    assert_eq!(client.reasoning_effort.as_deref(), Some("medium"));
    assert_eq!(client.reasoning_budget_tokens, Some(4096));
}

/// The Anthropic Messages request has no reasoning field: there a request
/// without reasoning is the ordinary one again, and nothing on the wire
/// says otherwise.
#[test]
fn an_anthropic_request_has_no_field_to_switch_reasoning_off_with() {
    let base = "http://127.0.0.1:1";
    assert_eq!(
        LlmClient::new(base).unwrap().without_reasoning_field(),
        Some("reasoning_effort: \"none\"")
    );
    assert_eq!(
        LlmClient::new(base)
            .unwrap()
            .with_api(ApiKind::AnthropicMessages)
            .without_reasoning_field(),
        None
    );
}

/// An endpoint already on `none` with no budget sends what a retry without
/// reasoning would send; only then is there nothing for the retry to
/// change.
#[test]
fn reasoning_is_off_only_when_nothing_but_none_is_configured() {
    let base = "http://127.0.0.1:1";
    assert!(
        !LlmClient::new(base).unwrap().reasoning_is_off(),
        "the provider's default is not off"
    );
    assert!(
        !LlmClient::new(base)
            .unwrap()
            .with_reasoning_effort(Some("low".into()))
            .reasoning_is_off()
    );
    assert!(
        !LlmClient::new(base)
            .unwrap()
            .with_reasoning_budget_tokens(Some(2048))
            .reasoning_is_off()
    );
    assert!(
        !LlmClient::new(base)
            .unwrap()
            .with_reasoning_effort(Some("none".into()))
            .with_reasoning_budget_tokens(Some(2048))
            .reasoning_is_off(),
        "a budget still asks for thinking"
    );
    assert!(
        LlmClient::new(base)
            .unwrap()
            .with_reasoning_effort(Some("none".into()))
            .reasoning_is_off()
    );
}

/// Each retry leaves one line for the caller, naming the failure and the
/// retry; asking again gets none. The interface decides how a retry is
/// shown — the client never prints, because in the TUI stderr is the screen.
#[tokio::test]
async fn each_retry_leaves_one_notice_for_the_caller() {
    let server = Scripted::spawn(vec![
        http("503 Service Unavailable", "", "busy"),
        http("200 OK", "", &ok_body("gg")),
        http("200 OK", "", &ok_body("again")),
    ]);
    let client = server.client();
    assert!(client.take_notices().is_empty(), "nothing has happened yet");
    client.send("m", &[], 16, &[]).await.unwrap();
    let notices = client.take_notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].contains("503 Service Unavailable"),
        "{}",
        notices[0]
    );
    assert!(notices[0].contains("retry 1 of 5"), "{}", notices[0]);
    assert!(
        client.take_notices().is_empty(),
        "a notice is handed over once"
    );

    // A request that needs no retry has nothing to say.
    client.send("m", &[], 16, &[]).await.unwrap();
    assert!(client.take_notices().is_empty());
}

// ------------------------------------------------------- send_summary

/// A one-request server: it records the JSON body it was sent and answers
/// with a plain completion, so the summary tests can read the request back.
struct Probe {
    base_url: String,
    body: std::sync::Arc<std::sync::Mutex<String>>,
    handle: std::thread::JoinHandle<()>,
}

fn read_one_request(stream: &mut std::net::TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let mut raw: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match std::io::Read::read(stream, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw).into_owned();
                if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                    let want = head
                        .lines()
                        .find_map(|l| {
                            let lower = l.to_ascii_lowercase();
                            lower
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if rest.len() >= want {
                        return rest.to_string();
                    }
                }
            }
        }
    }
    String::from_utf8_lossy(&raw).into_owned()
}

impl Probe {
    fn spawn() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let body = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let recorded = std::sync::Arc::clone(&body);
        let handle = std::thread::spawn(move || {
            let stream = match listener.incoming().next() {
                Some(Ok(stream)) => stream,
                _ => return,
            };
            let mut stream = stream;
            let request = read_one_request(&mut stream);
            *recorded
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = request;
            let answer = r#"{"choices":[{"message":{"role":"assistant","content":"SUMMARY"}}]}"#;
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                )
                .as_bytes(),
            );
        });
        Self {
            base_url,
            body,
            handle,
        }
    }

    fn request_body(self) -> String {
        let _ = self.handle.join();
        self.body
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// The summarize call is the cheap one: heavy reasoning drops to `low`, the
/// thinking budget and the output budget both cap, and an unconfigured
/// client still sends neither reasoning field.
#[tokio::test]
async fn send_summary_downgrades_reasoning_and_caps_the_budgets() {
    let probe = Probe::spawn();
    let client = LlmClient::new(&probe.base_url)
        .unwrap()
        .with_reasoning_effort(Some("high".into()))
        .with_reasoning_budget_tokens(Some(4096));
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("summarize this".into()),
        ..ChatMessage::default()
    }];
    let response = client
        .send_summary("test-model", &messages, 16384)
        .await
        .unwrap();
    assert_eq!(
        response.choices[0].message.content.as_deref(),
        Some("SUMMARY")
    );
    let body = probe.request_body();
    assert!(body.contains(r#""reasoning_effort":"low""#), "{body}");
    assert!(body.contains(r#""reasoning_budget_tokens":512"#), "{body}");
    assert!(body.contains(r#""max_tokens":4096"#), "{body}");
    assert!(!body.contains(r#""tools""#), "{body}");

    let probe = Probe::spawn();
    let client = LlmClient::new(&probe.base_url)
        .unwrap()
        .with_reasoning_effort(Some("medium".into()));
    client
        .send_summary("test-model", &messages, 1024)
        .await
        .unwrap();
    let body = probe.request_body();
    assert!(body.contains(r#""reasoning_effort":"low""#), "{body}");
    assert!(body.contains(r#""max_tokens":1024"#), "{body}");

    // Nothing configured: the fields stay absent, so a strict endpoint
    // never sees one it did not ask for.
    let probe = Probe::spawn();
    let client = LlmClient::new(&probe.base_url).unwrap();
    client
        .send_summary("test-model", &messages, 16384)
        .await
        .unwrap();
    let body = probe.request_body();
    assert!(!body.contains("reasoning_effort"), "{body}");
    assert!(!body.contains("reasoning_budget_tokens"), "{body}");
    assert!(body.contains(r#""max_tokens":4096"#), "{body}");
}
