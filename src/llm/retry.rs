//! The retry policy for LLM transport calls: what deserves another
//! attempt, and how long to wait before taking it.
//!
//! Every decision is a pure function of the attempt number, the HTTP
//! status, the transport error kind, and the request's `Retry-After` —
//! no clock, no sleep, no hidden state. That is what lets the policy be
//! tested on its own, and what keeps the client's retry loop a plain
//! `for` over decisions it can name.

use std::time::Duration;

/// Ceiling on a `Retry-After` the provider asks for. Only delta-seconds
/// is read (the HTTP-date form would mean parsing dates without a date
/// library, and providers in practice send seconds); a request past this
/// cap is answered with our own backoff rather than by sitting still.
pub(super) const RETRY_AFTER_CAP: Duration = Duration::from_secs(30);

/// The retry budget and what waiting costs. `max_retries` is configurable
/// (`max_retries` in config, `GG_MAX_RETRIES`); zero turns retrying off.
/// A zero `base_delay` — test-only — decides exactly and never sleeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub max_retries: u32,
    pub base_delay: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(500),
        }
    }
}

impl Backoff {
    /// Is retry number `nth` (0 for the first) inside the budget?
    pub fn allows(&self, nth: u32) -> bool {
        nth < self.max_retries
    }

    /// The wait before retry `nth`: doubling from `base_delay` plus a
    /// jitter of 0..25 %, so a fleet of clients woken by the same 429
    /// does not stampede back in lockstep. A `Retry-After` the provider
    /// named wins, up to [`RETRY_AFTER_CAP`].
    pub fn delay(&self, nth: u32, retry_after: Option<Duration>) -> Duration {
        match retry_after {
            Some(after) => after.min(RETRY_AFTER_CAP),
            None => {
                let doubled =
                    (self.base_delay.as_millis() as u64).saturating_mul(1u64 << nth.min(16));
                let window = doubled / 4;
                let jittered = if window == 0 {
                    doubled
                } else {
                    doubled.saturating_add(spread(nth) % window)
                };
                Duration::from_millis(jittered)
            }
        }
    }
}

/// The jitter amount, derived from the attempt number rather than from a
/// random source: a real delay, and still an assertable function.
fn spread(nth: u32) -> u64 {
    let mixed = 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(nth as u64 + 1);
    mixed >> 32
}

/// Whether another attempt could plausibly do better. Transport failures
/// are classified by kind, so no decision turns on an error string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Connection, request, or timeout failure before a response.
    Transport,
    /// A transient status, carrying the `Retry-After` the provider asked
    /// for (already parsed).
    Status { retry_after: Option<Duration> },
    /// A 4xx other than 408/429, an undecodable body, a half-sent body:
    /// asking again asks the same question and gets the same answer.
    Permanent,
}

impl Verdict {
    /// The wait before retry `nth`, or `None` when this failure ends the
    /// call. The whole retry decision in one pure step.
    pub fn retry_delay(self, backoff: &Backoff, nth: u32) -> Option<Duration> {
        if !backoff.allows(nth) {
            return None;
        }
        match self {
            Verdict::Transport => Some(backoff.delay(nth, None)),
            Verdict::Status { retry_after } => Some(backoff.delay(nth, retry_after)),
            Verdict::Permanent => None,
        }
    }
}

/// Classify a reqwest failure up to and including the send. Only failures
/// before the response head arrives reach here: once a body starts
/// streaming, the caller has already decided nothing will be re-asked.
pub(super) fn classify_err(err: &reqwest::Error) -> Verdict {
    // `is_body` before `is_request`: a body-send error is also a request
    // error, and half an upload is not a clean retry.
    if err.is_body() || err.is_decode() {
        return Verdict::Permanent;
    }
    if err.is_timeout() || err.is_connect() || err.is_request() {
        return Verdict::Transport;
    }
    Verdict::Permanent
}

/// Classify an HTTP status. 408 and 429 are transient by contract, 5xx
/// may be a provider having a bad minute; the rest of the 4xx family is
/// the request's own fault.
pub(super) fn classify_status(
    status: reqwest::StatusCode,
    retry_after: Option<Duration>,
) -> Verdict {
    let transient = status.is_server_error()
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
    if transient {
        Verdict::Status { retry_after }
    } else {
        Verdict::Permanent
    }
}

/// Parse a `Retry-After` header in delta-seconds, capped at
/// [`RETRY_AFTER_CAP`] on the way in (a huge value would overflow a
/// `Duration`). `None` means "no usable value" — the caller then takes
/// the ordinary backoff. The HTTP-date form is not read: it would mean
/// parsing dates without a date library, and providers send seconds.
pub(super) fn parse_retry_after(value: Option<&str>) -> Option<Duration> {
    let raw = value?.trim();
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Digits only: anything that does not fit `u128` is far past the cap,
    // so it is capped rather than thrown away as unparseable.
    let cap = u128::from(RETRY_AFTER_CAP.as_secs());
    let secs = raw.parse::<u128>().map_or(cap, |secs| secs.min(cap));
    Some(Duration::from_secs(secs as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backoff(max_retries: u32, base_ms: u64) -> Backoff {
        Backoff {
            max_retries,
            base_delay: Duration::from_millis(base_ms),
        }
    }

    #[test]
    fn defaults_are_three_retries_off_a_500ms_base() {
        let b = Backoff::default();
        assert_eq!(b.max_retries, 3);
        assert_eq!(b.base_delay, Duration::from_millis(500));
        assert!(b.allows(2) && !b.allows(3));
        assert!(!backoff(0, 500).allows(0), "zero retries means one attempt");
    }

    /// Delays double per attempt and stay inside the jitter window; a
    /// zero base decides exactly and never sleeps.
    #[test]
    fn delays_double_and_stay_within_the_jitter_window() {
        let b = backoff(3, 500);
        for (nth, base) in [(0, 500u64), (1, 1000), (2, 2000), (3, 4000)] {
            let got = b.delay(nth, None).as_millis();
            let window = base / 4;
            assert!(
                (base as u128..(base + window) as u128).contains(&got),
                "retry {nth}: {got} ms not in {base}..{}",
                base + window
            );
        }
        assert_eq!(backoff(3, 0).delay(2, None), Duration::ZERO);
    }

    #[test]
    fn retry_after_wins_and_is_capped() {
        let b = backoff(3, 500);
        assert_eq!(
            b.delay(0, Some(Duration::from_secs(7))),
            Duration::from_secs(7)
        );
        assert_eq!(b.delay(2, Some(Duration::from_secs(3600))), RETRY_AFTER_CAP);
        assert_eq!(b.delay(0, Some(Duration::ZERO)), Duration::ZERO);
    }

    #[test]
    fn retry_after_parses_seconds_only() {
        assert_eq!(parse_retry_after(Some("0")), Some(Duration::from_secs(0)));
        assert_eq!(
            parse_retry_after(Some(" 12 ")),
            Some(Duration::from_secs(12))
        );
        assert_eq!(
            parse_retry_after(Some("999999999999999999999")),
            Some(RETRY_AFTER_CAP),
            "an absurd request is capped, not overflowed"
        );
        assert_eq!(
            parse_retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT")),
            None
        );
        assert_eq!(parse_retry_after(Some("-5")), None);
        assert_eq!(parse_retry_after(None), None);
    }

    #[test]
    fn statuses_split_transient_from_permanent() {
        for code in [
            reqwest::StatusCode::REQUEST_TIMEOUT,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            reqwest::StatusCode::BAD_GATEWAY,
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            reqwest::StatusCode::GATEWAY_TIMEOUT,
        ] {
            assert_eq!(
                classify_status(code, None),
                Verdict::Status { retry_after: None },
                "{code} must be retryable"
            );
        }
        for code in [
            reqwest::StatusCode::BAD_REQUEST,
            reqwest::StatusCode::UNAUTHORIZED,
            reqwest::StatusCode::NOT_FOUND,
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            assert_eq!(
                classify_status(code, None),
                Verdict::Permanent,
                "{code} must not be retryable"
            );
        }
    }

    /// The decision the client asks on every failure: bounded by the
    /// budget, ended by permanence.
    #[test]
    fn the_budget_bounds_retries_and_permanence_ends_them() {
        let b = backoff(2, 0);
        assert!(Verdict::Transport.retry_delay(&b, 0).is_some());
        assert!(Verdict::Transport.retry_delay(&b, 1).is_some());
        assert_eq!(Verdict::Transport.retry_delay(&b, 2), None);
        assert_eq!(
            Verdict::Status {
                retry_after: Some(Duration::from_secs(4)),
            }
            .retry_delay(&b, 0),
            Some(Duration::from_secs(4))
        );
        assert_eq!(Verdict::Permanent.retry_delay(&b, 0), None);
    }

    /// A connection refused on a dead local port is the transport case
    /// the provider presets hit most — classified, not string-matched.
    #[tokio::test]
    async fn a_refused_connection_classifies_as_transport() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let err = reqwest::Client::new()
            .post(format!("http://{addr}/chat/completions"))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap_err();
        assert_eq!(classify_err(&err), Verdict::Transport, "{err:?}");
    }

    /// A whole-response timeout is a timeout, not a permanent failure.
    #[tokio::test]
    async fn a_client_timeout_classifies_as_transport() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // Accept and never answer: the client's own timer must fire.
            std::thread::sleep(std::time::Duration::from_millis(200));
            drop(stream);
        });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(20))
            .build()
            .unwrap();
        let err = client
            .post(format!("http://{addr}"))
            .send()
            .await
            .unwrap_err();
        assert_eq!(classify_err(&err), Verdict::Transport, "{err:?}");
        server.join().unwrap();
    }
}
