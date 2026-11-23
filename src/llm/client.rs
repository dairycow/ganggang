use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context;
use serde_json::Value;

use super::anthropic;
use super::retry::{self, Backoff, Verdict};
use super::stream::{self, StreamOutcome};
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
    /// The whole-response timeout carried on every request — response head
    /// AND body, so it bounds a stream too. Generations longer than a few
    /// minutes are the reason it is a setting (GAP 2), not a constant.
    request_timeout: Duration,
}

impl LlmClient {
    pub fn new(base_url: &str) -> anyhow::Result<Self> {
        let base_url = base_url.trim_end_matches('/').to_string();
        let session_header = is_opencode_url(&base_url).then(|| "x-opencode-session".to_string());
        Ok(Self {
            http: reqwest::Client::builder()
                // reqwest has NO timeout by default: a wedged model server would
                // hang gg forever. Two separate ones matter — connecting is a
                // different failure from a slow response. The whole-response
                // timeout is set per request instead (see `request_timeout`),
                // so it can be re-derived per attempt and configured.
                .connect_timeout(Duration::from_secs(10))
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
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
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

    /// Set the whole-response timeout (config `request_timeout_secs` /
    /// `GG_REQUEST_TIMEOUT`). It replaces the old fixed 300 s, which
    /// guillotined long generations mid-stream.
    pub fn with_request_timeout(mut self, secs: u64) -> Self {
        self.request_timeout = Duration::from_secs(secs.max(1));
        self
    }

    /// The whole-response timeout this client was built with — carried over
    /// when an endpoint switch rebuilds it, like the retry budget.
    pub fn request_timeout_secs(&self) -> u64 {
        self.request_timeout.as_secs()
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
                    self.request_timeout,
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
                    self.request_timeout,
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
    /// is the response the chunks assembled plus whether the wire said the
    /// stream finished ([`StreamOutcome`], GAP 2) — the response is what
    /// [`LlmClient::send`] would have returned, so callers do not change
    /// shape. The whole-response timeout applies here too, now spanning the
    /// whole streamed body (§17: everything that can hang has a number).
    ///
    /// `abort` is the running agent's stop request: the stream
    /// read polls it, so Esc does not have to wait for a generation to
    /// finish. An aborted read returns what assembled so far with
    /// `aborted: true` — never an error, never a retry.
    ///
    /// Retrying follows the same rules as [`LlmClient::send`] for the
    /// request itself: a 500 before a single byte of SSE is worth another
    /// attempt. Once the response head is in, the error is returned as it
    /// is — replaying a stream that already delivered a delta would show
    /// the user the same text twice (§17: never silently duplicate output).
    /// A stop request ends the retry loop outright: the user asked for no
    /// more requests, and none is made.
    pub async fn send_stream(
        &self,
        model: &str,
        messages: &[ChatMessage],
        max_tokens: u32,
        tools: &[Value],
        abort: &AtomicBool,
        on_delta: impl FnMut(stream::Delta) -> Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> anyhow::Result<StreamOutcome> {
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
                    self.request_timeout,
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
                    self.request_timeout,
                )?
            }
        };
        // The stop request is part of the send: it ends the wait for the
        // response head and rules out a retry.
        let response = self.attempts(&url, request, Some(abort)).await?;
        match self.api {
            ApiKind::OpenAiCompletions => stream::drain_sse(response, abort, on_delta).await,
            ApiKind::AnthropicMessages => {
                anthropic::stream::drain_sse(response, abort, on_delta).await
            }
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
        self.attempts(url, request, None).await
    }

    /// The retry loop both call paths share. `abort` is the streaming
    /// path's stop request: it ends the wait for the response head with
    /// [`ABORTED`] and rules out another attempt. `None` is the
    /// non-streaming path, which has no way to observe one mid-flight.
    async fn attempts(
        &self,
        url: &str,
        request: reqwest::Request,
        abort: Option<&AtomicBool>,
    ) -> anyhow::Result<reqwest::Response> {
        let mut nth = 0u32;
        loop {
            let outcome = match abort {
                // A stop request is honoured before the attempt: a request
                // the user has already disowned is not sent again.
                Some(flag) if flag.load(Ordering::Relaxed) => {
                    return Err(anyhow::anyhow!(ABORTED));
                }
                // Otherwise wait for the head — but not past the stop
                // request. Dropping the future cancels the in-flight
                // request, so the connection closes and the wait ends.
                Some(flag) => {
                    let attempt = self.attempt(url, replay(&request)?);
                    match stream::until_abort(flag, attempt).await {
                        Some(outcome) => outcome,
                        None => return Err(anyhow::anyhow!(ABORTED)),
                    }
                }
                None => self.attempt(url, replay(&request)?).await,
            };
            match outcome {
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

/// The whole-response timeout used when config says nothing.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// What a streaming call reports when the run's stop request ended it
/// before a response head arrived. It names itself as the user's stop, so
/// the error and the `[aborted by user]` entry the loop records for a
/// stopped run read as one event. Never retried.
const ABORTED: &str = "aborted by user";

/// Finish a request once, so every attempt replays the identical bytes.
/// `RequestBuilder::send` consumes the builder, and a retry has to be the
/// same request — same body, same headers — not a re-derivation of it.
/// The timeout rides the request (not the client) so the setting is a
/// number this function can be tested on.
fn build_request(
    builder: reqwest::RequestBuilder,
    timeout: Duration,
) -> anyhow::Result<(String, reqwest::Request)> {
    let request = builder
        .timeout(timeout)
        .build()
        .context("building request")?;
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

/// Streaming retry tests: the request half retries, the body half never
/// does, and a cut body is salvage rather than loss (GAP 2).
#[cfg(test)]
mod stream_tests;

#[cfg(test)]
mod tests;
