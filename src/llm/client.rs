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
                    reasoning_budget_tokens: self.reasoning_budget_tokens,
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
                    reasoning_budget_tokens: self.reasoning_budget_tokens,
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
}

/// The whole-response timeout used when config says nothing.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

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
