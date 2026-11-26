//! The retry loop behind both call paths: one request, made again while a
//! failure is worth another attempt, with the stop request wired in, and the
//! request-replay plumbing that keeps every attempt byte-identical.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context;

use super::{LlmClient, Refused};
use crate::llm::retry::{self, Verdict};
use crate::llm::stream;

impl LlmClient {
    /// One request, retried until the budget or the failure's nature says
    /// stop. The final error is the last attempt's, so what the provider
    /// actually said is what the user reads.
    pub(super) async fn post(
        &self,
        url: &str,
        request: reqwest::Request,
    ) -> anyhow::Result<reqwest::Response> {
        self.attempts(url, request, None).await
    }

    /// The retry loop both call paths share. `abort` is the streaming
    /// path's stop request: it ends the wait for the response head and the
    /// wait before a retry with [`ABORTED`], and rules out another
    /// attempt. `None` is the non-streaming path, whose caller races the
    /// whole call against the same flag instead.
    pub(super) async fn attempts(
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
                    // The wait before a retry belongs to the stop request
                    // too: a `Retry-After` can be half a minute, and the
                    // user who asked for no more requests should not sit
                    // it out.
                    match abort {
                        Some(flag) => {
                            if stream::until_abort(flag, tokio::time::sleep(wait))
                                .await
                                .is_none()
                            {
                                return Err(anyhow::anyhow!(ABORTED));
                            }
                        }
                        None => tokio::time::sleep(wait).await,
                    }
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
        let message = format!("POST {url} {}", status_detail(status, &body));
        // A 4xx is the server's answer to the request itself, so nothing
        // is retried. It is typed too: a caller with a fallback for a
        // refused request has to tell it from a transport failure.
        let error = if status.is_client_error() {
            anyhow::Error::new(Refused(message))
        } else {
            anyhow::anyhow!(message)
        };
        Err(Failure {
            retry: retry::classify_status(status, retry_after),
            reason: status.to_string(),
            error,
        })
    }

    /// The retry decision for one failed attempt: `None` ends the call,
    /// otherwise the wait before the retry — and a line for the user, kept
    /// until the caller asks for it, that says what failed and what is
    /// being done about it.
    fn retry_in(&self, failure: &Failure, nth: u32, url: &str) -> Option<Duration> {
        let wait = failure.retry.retry_delay(&self.retry, nth)?;
        self.notices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!(
                "POST {url}: {} — retry {} of {} in {} ms",
                failure.reason,
                nth + 1,
                self.retry.max_retries,
                wait.as_millis()
            ));
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
pub(super) fn build_request(
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
pub(super) fn replay(request: &reqwest::Request) -> anyhow::Result<reqwest::Request> {
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
