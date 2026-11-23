use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use anyhow::Context;
use serde_json::Value;

use super::anthropic;
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
    reasoning_budget_tokens: Option<u32>,
    session_id: String,
    session_header: Option<String>,
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
            reasoning_budget_tokens: None,
            session_id: uuid::Uuid::new_v4().hyphenated().to_string(),
            session_header,
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

    pub fn with_reasoning_budget_tokens(mut self, budget: Option<u32>) -> Self {
        self.reasoning_budget_tokens = budget.filter(|&n| n > 0);
        self
    }

    pub fn with_session_id(mut self, session_id: String) -> Self {
        self.session_id = session_id;
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
    pub async fn send(
        &self,
        model: &str,
        messages: &[ChatMessage],
        max_tokens: u32,
        tools: &[Value],
    ) -> anyhow::Result<ChatResponse> {
        match self.api {
            ApiKind::OpenAiCompletions => {
                let url = format!("{}/chat/completions", self.base_url);
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
                let request = self.apply_auth(self.http.post(&url).json(&body));
                let resp = request
                    .send()
                    .await
                    .with_context(|| format!("POST {url}"))?;
                let resp = check_status(resp, &url).await?;
                resp.json()
                    .await
                    .context("response was not JSON matching ChatResponse")
            }
            ApiKind::AnthropicMessages => {
                let url = anthropic::messages_url(&self.base_url);
                let body = anthropic::build_request(model, messages, max_tokens, tools, false);
                let request = self
                    .apply_auth(
                        self.http
                            .post(&url)
                            .header("anthropic-version", anthropic::ANTHROPIC_VERSION),
                    )
                    .json(&body);
                let resp = request
                    .send()
                    .await
                    .with_context(|| format!("POST {url}"))?;
                let resp = check_status(resp, &url).await?;
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
    pub async fn send_stream(
        &self,
        model: &str,
        messages: &[ChatMessage],
        max_tokens: u32,
        tools: &[Value],
        on_delta: impl FnMut(stream::Delta) -> Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> anyhow::Result<ChatResponse> {
        match self.api {
            ApiKind::OpenAiCompletions => {
                let url = format!("{}/chat/completions", self.base_url);
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
                let request = self.apply_auth(self.http.post(&url).json(&body));
                let resp = request
                    .send()
                    .await
                    .with_context(|| format!("POST {url}"))?;
                let resp = check_status(resp, &url).await?;
                stream::drain_sse(resp, on_delta).await
            }
            ApiKind::AnthropicMessages => {
                let url = anthropic::messages_url(&self.base_url);
                let body = anthropic::build_request(model, messages, max_tokens, tools, true);
                let request = self
                    .apply_auth(
                        self.http
                            .post(&url)
                            .header("anthropic-version", anthropic::ANTHROPIC_VERSION),
                    )
                    .json(&body);
                let resp = request
                    .send()
                    .await
                    .with_context(|| format!("POST {url}"))?;
                let resp = check_status(resp, &url).await?;
                anthropic::stream::drain_sse(resp, on_delta).await
            }
        }
    }
}

async fn check_status(resp: reqwest::Response, url: &str) -> anyhow::Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let trimmed = body.trim();
    if trimmed.is_empty() {
        anyhow::bail!("POST {url} failed ({status})");
    }
    let excerpt: String = trimmed.chars().take(1024).collect();
    anyhow::bail!("POST {url} failed ({status}): {excerpt}");
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
    use std::sync::{Arc, Mutex};

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

    #[tokio::test]
    async fn reasoning_effort_and_budget_are_sent_when_configured() {
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
            .with_reasoning_effort(Some("medium".into()))
            .with_reasoning_budget_tokens(Some(4096));
        let _ = client.send("m", &[], 16, &[]).await.unwrap();
        let raw = seen.lock().unwrap().clone();
        let body = raw.split("\r\n\r\n").nth(1).unwrap();
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["reasoning_effort"], "medium");
        assert_eq!(v["reasoning_budget_tokens"], 4096);
    }
}
