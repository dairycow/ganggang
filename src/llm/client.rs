use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Context;
use serde_json::Value;

use super::anthropic;
use super::retry::Backoff;
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
    reasoning_budget_tokens: Option<u32>,
    session_id: String,
    session_header: Option<String>,
    /// The retry budget for transient transport failures (§ client docs).
    retry: Backoff,
    /// The whole-response timeout carried on every request — response head
    /// AND body, so it bounds a stream too. Generations longer than a few
    /// minutes are the reason it is a setting (GAP 2), not a constant.
    request_timeout: Duration,
    /// Retries made since the caller last asked ([`Self::take_notices`]).
    /// The client does not print them: in the TUI stderr is the screen.
    notices: std::sync::Mutex<Vec<String>>,
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
            reasoning_budget_tokens: None,
            session_id: uuid::Uuid::new_v4().hyphenated().to_string(),
            session_header,
            retry: Backoff::default(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            notices: std::sync::Mutex::new(Vec::new()),
        })
    }

    pub fn from_endpoint(endpoint: &crate::config::Endpoint) -> anyhow::Result<Self> {
        Ok(Self::new(&endpoint.base_url)?
            .with_api(endpoint.api)
            .with_auth(endpoint.auth)
            .with_api_key(endpoint.api_key.clone())
            .with_reasoning_effort(endpoint.reasoning_effort.clone())
            .with_reasoning_budget_tokens(endpoint.reasoning_budget_tokens))
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
        self.send_with_reasoning(
            model,
            messages,
            max_tokens,
            tools,
            self.reasoning_effort.as_deref(),
            self.reasoning_budget_tokens,
        )
        .await
    }

    /// The summarize call compaction makes: the same wire with cheaper
    /// knobs. It carries no tools, its `max_tokens` is capped at
    /// [`SUMMARY_MAX_TOKENS`], a heavy configured reasoning effort is sent
    /// as `"low"`, and a configured token budget is capped at
    /// [`SUMMARY_REASONING_BUDGET_TOKENS`].
    ///
    /// Reasoning is adjusted only where it was configured: an endpoint that
    /// was never sent a reasoning field is not sent one here either, so
    /// strict servers keep accepting the request.
    pub async fn send_summary(
        &self,
        model: &str,
        messages: &[ChatMessage],
        max_tokens: u32,
    ) -> anyhow::Result<ChatResponse> {
        let effort = match self.reasoning_effort.as_deref() {
            Some(effort)
                if HEAVY_REASONING_EFFORTS
                    .contains(&effort.trim().to_ascii_lowercase().as_str()) =>
            {
                Some("low")
            }
            other => other,
        };
        self.send_with_reasoning(
            model,
            messages,
            max_tokens.clamp(1, SUMMARY_MAX_TOKENS),
            &[],
            effort,
            self.reasoning_budget_tokens
                .map(|budget| budget.min(SUMMARY_REASONING_BUDGET_TOKENS)),
        )
        .await
    }

    /// [`send`](Self::send) with reasoning switched off for this one request:
    /// `reasoning_effort: "none"` and no budget, whatever the endpoint
    /// configured. It is the retry a turn earns when reasoning used up the
    /// whole `max_tokens`; every later request goes out as configured again
    /// unless it goes through this method too.
    pub async fn send_without_reasoning(
        &self,
        model: &str,
        messages: &[ChatMessage],
        max_tokens: u32,
        tools: &[Value],
    ) -> anyhow::Result<ChatResponse> {
        self.send_with_reasoning(
            model,
            messages,
            max_tokens,
            tools,
            Some(REASONING_OFF),
            None,
        )
        .await
    }

    /// Whether every request already goes out with reasoning off
    /// ([`send_without_reasoning`](Self::send_without_reasoning) would send
    /// the same one again), so a retry without reasoning has nothing to
    /// change.
    pub fn reasoning_is_off(&self) -> bool {
        self.reasoning_effort
            .as_deref()
            .is_some_and(|effort| effort.trim().eq_ignore_ascii_case(REASONING_OFF))
            && self.reasoning_budget_tokens.is_none()
    }

    /// The non-streaming send both call paths share: build the request for
    /// the endpoint's API, send it, parse the response. [`LlmClient::send`]
    /// passes the client's own reasoning settings; [`LlmClient::send_summary`]
    /// passes the cheaper pair.
    async fn send_with_reasoning(
        &self,
        model: &str,
        messages: &[ChatMessage],
        max_tokens: u32,
        tools: &[Value],
        reasoning_effort: Option<&str>,
        reasoning_budget_tokens: Option<u32>,
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
                    reasoning_effort,
                    reasoning_budget_tokens,
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
        let (url, request) = self.stream_request(
            model,
            messages,
            max_tokens,
            tools,
            self.reasoning_effort.as_deref(),
            self.reasoning_budget_tokens,
        )?;
        self.drain_stream(&url, request, abort, on_delta).await
    }

    /// [`send_stream`](Self::send_stream) with reasoning switched off for
    /// this one request, as [`send_without_reasoning`](Self::send_without_reasoning)
    /// does for the plain one.
    pub async fn send_stream_without_reasoning(
        &self,
        model: &str,
        messages: &[ChatMessage],
        max_tokens: u32,
        tools: &[Value],
        abort: &AtomicBool,
        on_delta: impl FnMut(stream::Delta) -> Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> anyhow::Result<StreamOutcome> {
        let (url, request) = self.stream_request(
            model,
            messages,
            max_tokens,
            tools,
            Some(REASONING_OFF),
            None,
        )?;
        self.drain_stream(&url, request, abort, on_delta).await
    }

    /// The streaming request for the endpoint's API, carrying the call's
    /// own reasoning pair: the endpoint's, or the pair a retry without
    /// reasoning passes.
    fn stream_request(
        &self,
        model: &str,
        messages: &[ChatMessage],
        max_tokens: u32,
        tools: &[Value],
        reasoning_effort: Option<&str>,
        reasoning_budget_tokens: Option<u32>,
    ) -> anyhow::Result<(String, reqwest::Request)> {
        match self.api {
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
                    reasoning_effort,
                    reasoning_budget_tokens,
                };
                build_request(
                    self.apply_auth(
                        self.http
                            .post(format!("{}/chat/completions", self.base_url))
                            .json(&body),
                    ),
                    self.request_timeout,
                )
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
                )
            }
        }
    }

    /// Send one built streaming request and assemble its answer.
    async fn drain_stream(
        &self,
        url: &str,
        request: reqwest::Request,
        abort: &AtomicBool,
        on_delta: impl FnMut(stream::Delta) -> Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> anyhow::Result<StreamOutcome> {
        // The stop request is part of the send: it ends the wait for the
        // response head and rules out a retry.
        let response = self.attempts(url, request, Some(abort)).await?;
        match self.api {
            ApiKind::OpenAiCompletions => stream::drain_sse(response, abort, on_delta).await,
            ApiKind::AnthropicMessages => {
                anthropic::stream::drain_sse(response, abort, on_delta).await
            }
        }
    }
}

/// The whole-response timeout used when config says nothing.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// The summarize call's output cap: a continuation summary is short, and a
/// large `max_tokens` is what makes a small request slow.
const SUMMARY_MAX_TOKENS: u32 = 4096;
/// The summarize call's thinking cap, for endpoints that budget it in tokens.
const SUMMARY_REASONING_BUDGET_TOKENS: u32 = 512;
/// Configured reasoning efforts worth pulling down for a summarize call.
/// `none` and `low` are already cheap, and an unconfigured client sends
/// neither field at all.
const HEAVY_REASONING_EFFORTS: [&str; 4] = ["medium", "high", "xhigh", "max"];

/// The `reasoning_effort` that switches a model's thinking off, as
/// [`LlmClient::send_without_reasoning`] and
/// [`LlmClient::send_stream_without_reasoning`] send it.
const REASONING_OFF: &str = "none";

/// The server's answer to a request it refused to serve: a 4xx status with
/// its body. `Display` is the error text; a caller with a fallback for a
/// refused request tells it from a transport failure with [`is_refusal`].
#[derive(Debug)]
pub struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// Whether an error from the client is the server's [`Refused`] answer to
/// the request itself, rather than a failure to reach or read it.
pub fn is_refusal(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Refused>().is_some()
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

mod attempt;
mod builder;

use attempt::build_request;

#[cfg(test)]
mod tests;
