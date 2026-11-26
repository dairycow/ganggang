//! The client's setters: the endpoint's settings, applied one `with_*` at a
//! time to the value `new`/`from_endpoint` built, plus the two figures an
//! endpoint switch carries over.

use std::time::Duration;

use super::LlmClient;
use crate::llm::ApiKind;
use crate::llm::AuthStyle;

impl LlmClient {
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

    /// The retries made since this was last asked, oldest first — one line
    /// each, saying what failed, which retry is coming and how long it
    /// waits. For the caller to pass on to the user through whatever
    /// interface it owns; nothing is printed here.
    pub fn take_notices(&self) -> Vec<String> {
        let mut notices = self.notices.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *notices)
    }

    /// Test seam: decide like production, never sleep.
    #[cfg(test)]
    pub(in crate::llm) fn with_retry_delay(mut self, base: Duration) -> Self {
        self.retry.base_delay = base;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_session_header(mut self, header: Option<String>) -> Self {
        self.session_header = header;
        self
    }
}
