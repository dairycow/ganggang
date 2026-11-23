use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use anyhow::Context;
use serde_json::Value;

use super::anthropic;
use super::retry::{self, Backoff, Verdict};
use super::stream;
use super::types::{ChatMessage, ChatRequest, ChatResponse, StreamOptions};
use super::{ApiKind, AuthStyle};

/// One client per process, reused across every turn of the agent loop.
/// reqwest::Client owns a connection pool; building it per request throws that
/// pooling away, so M2's loop calls .send() on this same value repeatedly.
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    api: ApiKind,
    auth: AuthStyle,
    api_key: Option<String>,
    reasoning_effort: Option<String>,
    session_id: String,
    session_header: Option<String>,
    /// The retry budget for transient transport failures (§ client docs).
    retry: Backoff,
}

impl LlmClient {
    pub fn new(base_url: &str) -> anyhow::Result<Self> {
        let base_url = base_url.trim_end_matches('/').to_string();
        let session_header = is_opencode_url(&base_url).then(|| "x-opencode-session".to_string());
        Ok(Self {
            http: reqwest::Client::builder()
                // reqwest has NO timeout by default: a wedged model server would
                // hang gg forever. Two separate ones matter — connecting is a
                // different failure from a slow response.
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(300))
                .user_agent(concat!("ganggang/", env!("CARGO_PKG_VERSION")))
                .build()
                .context("building http client")?,
            // Trailing slash in GG_BASE_URL would give us "//chat/completions".
            base_url,
            api: ApiKind::OpenAiCompletions,
            auth: AuthStyle::Auto,
            api_key: None,
            reasoning_effort: None,
            session_id: uuid::Uuid::new_v4().hyphenated().to_string(),
            session_header,
            retry: Backoff::default(),
        })
    }

    pub fn from_endpoint(endpoint: &crate::config::Endpoint) -> anyhow::Result<Self> {
        Ok(Self::new(&endpoint.base_url)?
            .with_api(endpoint.api)
            .with_auth(endpoint.auth)
            .with_api_key(endpoint.api_key.clone())
            .with_reasoning_effort(endpoint.reasoning_effort.clone()))
    }

    pub fn with_api(mut self, api: ApiKind) -> Self {
        self.api = api;
        self
    }

    pub fn with_auth(mut self, auth: AuthStyle) -> Self {
        self.auth = auth;
        self
    }

    /// Attach an API key, sent according to [`AuthStyle`].
    pub fn with_api_key(mut self, api_key: Option<String>) -> Self {
        self.api_key = api_key;
        self
    }

    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    pub fn with_session_id(mut self, session_id: String) -> Self {
        self.session_id = session_id;
        self
    }

    /// Set the retry budget (`max_retries` from config / `GG_MAX_RETRIES`).
    pub fn with_retry(mut self, max_retries: u32) -> Self {
        self.retry.max_retries = max_retries;
        self
    }

    /// The retry budget this client was built with — carried over when an
    /// endpoint switch rebuilds it.
    pub fn max_retries(&self) -> u32 {
        self.retry.max_retries
    }

    /// Test seam: decide like production, never sleep.
    #[cfg(test)]
    fn with_retry_delay(mut self, base: Duration) -> Self {
        self.retry.base_delay = base;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_session_header(mut self, header: Option<String>) -> Self {
        self.session_header = header;
        self
    }

    fn apply_auth(&self, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(header) = &self.session_header {
            request = request.header(header, &self.session_id);
            if header == "x-opencode-session" {
                request = request.header("x-opencode-client", "ganggang");
            }
        }
        let Some(key) = &self.api_key else {
            return request;
        };
        match (self.auth, self.api) {
            (AuthStyle::Bearer, _) | (AuthStyle::Auto, ApiKind::OpenAiCompletions) => {
                request.bearer_auth(key)
            }
            (AuthStyle::XApiKey, _) | (AuthStyle::Auto, ApiKind::AnthropicMessages) => {
                request.header("x-api-key", key)
            }
        }
    }

    /// Borrows the history (&[ChatMessage]) and tool declarations rather than
    /// taking ownership: the caller keeps growing the session, and the
    /// registry stays the single source of tool schemas. An empty `tools`
    /// slice omits the key from the request entirely.
    ///
    /// Transient transport failures are retried with bounded exponential
    /// backoff ([`retry`]); a 4xx other than 408/429 fails at once, because
    /// a second identical request gets a second identical no.
    pub async fn send(
        &self,
        model: &str,
        messages: &[ChatMessage],
        max_tokens: u32,
        tools: &[Value],
    ) -> anyhow::Result<ChatResponse> {
        match self.api {
            ApiKind::OpenAiCompletions => {
                let body = ChatRequest {
                    model,
                    messages,
                    max_tokens,
                    tools,
                    stream: None,
                    stream_options: None,
                    reasoning_effort: self.reasoning_effort.as_deref(),
                };
                let (url, request) = build_request(
                    self.apply_auth(
                        self.http
                            .post(format!("{}/chat/completions", self.base_url))
                            .json(&body),
                    ),
                )?;
                let resp = self.post(&url, request).await?;
                resp.json()
                    .await
                    .context("response was not JSON matching ChatResponse")
            }
            ApiKind::AnthropicMessages => {
                let body = anthropic::build_request(model, messages, max_tokens, tools, false);
                let (url, request) = build_request(
                    self.apply_auth(
                        self.http
                            .post(anthropic::messages_url(&self.base_url))
                            .header("anthropic-version", anthropic::ANTHROPIC_VERSION),
                    )
                    .json(&body),
                )?;
                let resp = self.post(&url, request).await?;
                let raw: Value = resp
                    .json()
                    .await
                    .context("response was not JSON matching Anthropic Messages")?;
                anthropic::parse_response(raw)
            }
        }
    }

    /// Streaming variant of [`LlmClient::send`] (spec §15, M5): the request
    /// carries `stream: true`, deltas (text and thinking, M7.3) are
    /// delivered through `on_delta` as they arrive, and the return value
    /// is the response the chunks assembled into — identical to what
    /// [`LlmClient::send`] would have returned, so callers do not change
    /// shape. The client's 300 s request timeout still applies, now
    /// spanning the whole streamed body (§17: everything that can hang
    /// has a number).
    ///
    /// Retrying follows the same rules as [`LlmClient::send`] for the
    /// request itself: a 500 before a single byte of SSE is worth another
    /// attempt. Once the response head is in, the error is returned as it
    /// is — replaying a stream that already delivered a delta would show
    /// the user the same text twice (§17: never silently duplicate output).
    pub async fn send_stream(
        &self,
        model: &str,
        messages: &[ChatMessage],
        max_tokens: u32,
        tools: &[Value],
        on_delta: impl FnMut(stream::Delta) -> Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> anyhow::Result<ChatResponse> {
        let (url, request) = match self.api {
            ApiKind::OpenAiCompletions => {
                let body = ChatRequest {
                    model,
                    messages,
                    max_tokens,
                    tools,
                    stream: Some(true),
                    stream_options: Some(StreamOptions {
                        include_usage: true,
                    }),
                    reasoning_effort: self.reasoning_effort.as_deref(),
                };
                build_request(
                    self.apply_auth(
                        self.http
                            .post(format!("{}/chat/completions", self.base_url))
                            .json(&body),
                    ),
                )?
            }
            ApiKind::AnthropicMessages => {
                let body = anthropic::build_request(model, messages, max_tokens, tools, true);
                build_request(
                    self.apply_auth(
                        self.http
                            .post(anthropic::messages_url(&self.base_url))
                            .header("anthropic-version", anthropic::ANTHROPIC_VERSION),
                    )
                    .json(&body),
                )?
            }
        };
        let response = self.post(&url, request).await?;
        match self.api {
            ApiKind::OpenAiCompletions => stream::drain_sse(response, on_delta).await,
            ApiKind::AnthropicMessages => anthropic::stream::drain_sse(response, on_delta).await,
        }
    }

    /// One request, retried until the budget or the failure's nature says
    /// stop. The final error is the last attempt's, so what the provider
    /// actually said is what the user reads.
    async fn post(
        &self,
        url: &str,
        request: reqwest::Request,
    ) -> anyhow::Result<reqwest::Response> {
        let mut nth = 0u32;
        loop {
            match self.attempt(url, replay(&request)?).await {
                Ok(response) => return Ok(response),
                Err(failure) => {
                    let Some(wait) = self.retry_in(&failure, nth, url) else {
                        return Err(failure.error);
                    };
                    tokio::time::sleep(wait).await;
                    nth += 1;
                }
            }
        }
    }

    /// One HTTP attempt: send, and classify the response head. Bodies are
    /// the caller's business — but an error body is read here, because the
    /// providers put the real reason in it and a retry warning and the final
    /// error both want it.
    async fn attempt(
        &self,
        url: &str,
        request: reqwest::Request,
    ) -> Result<reqwest::Response, Failure> {
        let response = match self.http.execute(request).await {
            Ok(response) => response,
            Err(err) => {
                // The kind decides the retry; the transport's own words are
                // the reason shown, so a timeout and a refused connection
                // do not read the same.
                let reason = if err.is_timeout() {
                    "timed out"
                } else if err.is_connect() {
                    "connection failed"
                } else {
                    "request failed"
                };
                return Err(Failure {
                    retry: retry::classify_err(&err),
                    reason: reason.to_string(),
                    error: anyhow::Error::from(err).context(format!("POST {url}")),
                });
            }
        };
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let retry_after = retry::parse_retry_after(
            response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
        );
        let body = response.text().await.unwrap_or_default();
        Err(Failure {
            retry: retry::classify_status(status, retry_after),
            reason: status.to_string(),
            error: anyhow::anyhow!("POST {url} {}", status_detail(status, &body)),
        })
    }

    /// The retry decision for one failed attempt: `None` ends the call,
    /// otherwise the warning the user sees and the wait before the retry.
    /// Retries are visible on stderr, so no layer above the client has to
    /// learn they happened.
    fn retry_in(&self, failure: &Failure, nth: u32, url: &str) -> Option<Duration> {
        let wait = failure.retry.retry_delay(&self.retry, nth)?;
        eprintln!(
            "warning: POST {url}: {} — retry {} of {} in {} ms",
            failure.reason,
            nth + 1,
            self.retry.max_retries,
            wait.as_millis()
        );
        Some(wait)
    }
}

/// A failed attempt: how (or whether) it may be retried, the short reason
/// for the retry warning, and the error to fail with if it is not.
struct Failure {
    retry: Verdict,
    reason: String,
    error: anyhow::Error,
}

/// Finish a request once, so every attempt replays the identical bytes.
/// `RequestBuilder::send` consumes the builder, and a retry has to be the
/// same request — same body, same headers — not a re-derivation of it.
fn build_request(builder: reqwest::RequestBuilder) -> anyhow::Result<(String, reqwest::Request)> {
    let request = builder.build().context("building request")?;
    let url = request.url().to_string();
    Ok((url, request))
}

/// Clone a request for another attempt. reqwest replays in-memory bodies,
/// which is all this client ever sends (JSON); the error is here for the
/// day that stops being true.
fn replay(request: &reqwest::Request) -> anyhow::Result<reqwest::Request> {
    request
        .try_clone()
        .context("the request body cannot be replayed for a retry")
}

/// The `failed (status): body` tail of an error message. The body is
/// capped, not dropped: providers put the actionable reason in it.
fn status_detail(status: reqwest::StatusCode, body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return format!("failed ({status})");
    }
    let excerpt: String = trimmed.chars().take(1024).collect();
    format!("failed ({status}): {excerpt}")
}

fn is_opencode_url(base_url: &str) -> bool {
    let Some((_, rest)) = base_url.split_once("://") else {
        return false;
    };
    let host = rest
        .split(['/', ':', '?'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    host == "opencode.ai" || host.ends_with(".opencode.ai")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// A scripted server that answers one raw response per connection and
    /// counts the connections it accepted — the attempt count, asserted
    /// rather than assumed. `Connection: close` on every response keeps
    /// connections and responses in step, retries included.
    struct Scripted {
        base_url: String,
        attempts: Arc<AtomicUsize>,
    }

    impl Scripted {
        fn spawn(responses: Vec<String>) -> Self {
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
        fn client(&self) -> LlmClient {
            LlmClient::new(&self.base_url)
                .unwrap()
                .with_retry_delay(Duration::ZERO)
        }
    }

    fn read_request_head(stream: &mut std::net::TcpStream) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0, "connection closed before headers finished");
            buf.extend_from_slice(&chunk[..n]);
        }
    }

    fn http(status: &str, extra_headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn ok_body(text: &str) -> String {
        format!(
            r##"{{"choices":[{{"message":{{"role":"assistant","content":"{text}"}},"finish_reason":"stop"}}]}}"##
        )
    }

    fn sse(text: &str) -> String {
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

    /// Streaming retries the initial request too: a 500 before any SSE is
    /// the same transient failure it always was.
    #[tokio::test]
    async fn a_stream_that_starts_with_500_is_retried() {
        let server = Scripted::spawn(vec![
            http("500 Internal Server Error", "", "nope"),
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{}",
                sse("streamed")
            ),
        ]);
        let resp = server
            .client()
            .send_stream("m", &[], 64, &[], |_| Box::pin(async {}))
            .await
            .unwrap();
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("streamed"));
        assert_eq!(server.attempts.load(Ordering::SeqCst), 2);
    }

    /// A stream cut after it delivered a delta is not retried: the caller
    /// has already shown that text, so a second attempt would show it
    /// twice. The error stays the stream's own.
    #[tokio::test]
    async fn a_stream_cut_after_a_delta_is_not_retried() {
        let cut = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"half\"}}]}\n\n";
        let server = Scripted::spawn(vec![cut.to_string(), cut.to_string()]);
        let deltas = Arc::new(Mutex::new(Vec::new()));
        let sink = deltas.clone();
        let err = server
            .client()
            .send_stream("m", &[], 64, &[], move |delta| {
                let sink = sink.clone();
                Box::pin(async move { sink.lock().unwrap().push(delta) })
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("without [DONE]"), "{err:#}");
        assert_eq!(
            *deltas.lock().unwrap(),
            vec![stream::Delta::Text("half".into())],
            "the delta reached the caller exactly once"
        );
        assert_eq!(server.attempts.load(Ordering::SeqCst), 1);
    }
}
