//! Streaming client tests: the request half of a stream retries like any
//! other request, and so does a stream that fails before it shows
//! anything; a body cut after a delta does not, and is salvage rather
//! than loss (GAP 2).

use std::future::Future;
use std::io::Write as _;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::LlmClient;
use super::stream;
use super::tests::{Scripted, http, never_abort, read_request_head, sse};

/// Streaming retries the initial request too: a 500 before any SSE is the
/// same transient failure it always was.
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
        .send_stream("m", &[], 64, &[], &never_abort(), |_| Box::pin(async {}))
        .await
        .unwrap();
    assert!(resp.complete, "[DONE] makes it a complete stream");
    assert_eq!(
        resp.response.choices[0].message.content.as_deref(),
        Some("streamed")
    );
    assert_eq!(server.attempts.load(Ordering::SeqCst), 2);
}

/// A stream cut after it delivered a delta is not retried (the caller
/// has already shown that text) and no longer discards it: the partial
/// answer comes back with `complete: false` for the loop to continue
/// from (GAP 2).
#[tokio::test]
async fn a_stream_cut_after_a_delta_is_not_retried_but_is_salvaged() {
    let cut = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"half\"}}]}\n\n";
    let server = Scripted::spawn(vec![cut.to_string(), cut.to_string()]);
    let deltas = Arc::new(Mutex::new(Vec::new()));
    let sink = deltas.clone();
    let outcome = server
        .client()
        .send_stream("m", &[], 64, &[], &never_abort(), move |delta| {
            let sink = sink.clone();
            Box::pin(async move { sink.lock().unwrap().push(delta) })
        })
        .await
        .unwrap();
    assert!(!outcome.complete, "no [DONE], so not complete");
    assert!(!outcome.aborted, "the wire cut it, the user did not");
    assert_eq!(
        outcome.response.choices[0].message.content.as_deref(),
        Some("half")
    );
    assert_eq!(
        *deltas.lock().unwrap(),
        vec![stream::Delta::Text("half".into())],
        "the delta reached the caller exactly once"
    );
    assert_eq!(server.attempts.load(Ordering::SeqCst), 1);
}

/// The configured timeout really does bound the streamed body: a server
/// that sends one delta and then stalls is cut off at the deadline, and
/// the stalled read is an error (an endless silence is not a partial
/// answer to continue from). This is what `request_timeout_secs` buys, and
/// what the old fixed 300 s priced out.
#[tokio::test]
async fn a_stalled_stream_body_hits_the_configured_deadline() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request_head(&mut stream);
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"one\"}}]}\n\n",
            )
            .unwrap();
        // Stall past the deadline, then leave — the read must not wait.
        std::thread::sleep(std::time::Duration::from_secs(5));
        drop(stream);
    });

    let started = std::time::Instant::now();
    let err = LlmClient::new(&format!("http://{addr}"))
        .unwrap()
        .with_retry(0)
        .with_request_timeout(1)
        .send_stream("m", &[], 64, &[], &never_abort(), |_| Box::pin(async {}))
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(4), "{elapsed:?}");
    assert!(err.to_string().contains("reading SSE stream"), "{err:#}");
}

/// Esc while a stream is still running: the read polls the run's
/// stop request, so a stalled body ends at the next poll — not at the
/// request timeout — and the text already streamed in is kept.
#[tokio::test]
async fn aborting_a_stalled_stream_ends_it_at_the_poll_not_the_timeout() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request_head(&mut stream);
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"one\"}}]}\n\n",
            )
            .unwrap();
        // Stay connected and silent: only the abort flag can end the read.
        std::thread::sleep(std::time::Duration::from_secs(30));
        drop(stream);
    });

    let abort = Arc::new(AtomicBool::new(false));
    let flag = abort.clone();
    let setter = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        flag.store(true, Ordering::Relaxed);
    });

    // A timeout far longer than the wait for the stop request, so passing
    // this assertion can only mean the flag ended the call.
    let started = std::time::Instant::now();
    let outcome = LlmClient::new(&format!("http://{addr}"))
        .unwrap()
        .with_retry(0)
        .with_request_timeout(60)
        .send_stream("m", &[], 64, &[], &abort, |_| Box::pin(async {}))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    setter.await.unwrap();

    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    assert!(outcome.aborted, "the user stopped the read");
    assert!(!outcome.complete, "no [DONE] was seen");
    assert_eq!(
        outcome.response.choices[0].message.content.as_deref(),
        Some("one"),
        "what streamed in before the stop request survives"
    );
}

/// A stop request that lands before the response head: the wait is
/// abandoned, the connection closes, and the caller gets the abort error
/// rather than a transport failure — and no retry is attempted.
#[tokio::test]
async fn aborting_before_the_response_head_is_an_abort_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted = attempts.clone();
    std::thread::spawn(move || {
        // Accept and never answer: the client's own timeout would need a
        // minute, so an early return can only be the stop request.
        let (stream, _) = listener.accept().unwrap();
        counted.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_secs(30));
        drop(stream);
    });

    let abort = Arc::new(AtomicBool::new(false));
    let flag = abort.clone();
    let setter = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        flag.store(true, Ordering::Relaxed);
    });

    let started = std::time::Instant::now();
    let err = LlmClient::new(&format!("http://{addr}"))
        .unwrap()
        .with_retry(3)
        .with_request_timeout(60)
        .send_stream("m", &[], 64, &[], &abort, |_| Box::pin(async {}))
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    setter.await.unwrap();

    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    assert_eq!(err.to_string(), "aborted by user", "{err:#}");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "a stop request is never retried"
    );
}

/// The wait before a retry belongs to the stop request too: a long
/// `Retry-After` is not sat out once the user has asked for no more
/// requests, and none is made.
#[tokio::test]
async fn a_stop_request_ends_a_long_retry_after_wait() {
    let server = Scripted::spawn(vec![http(
        "429 Too Many Requests",
        "Retry-After: 30\r\n",
        "slow down",
    )]);
    let flag = Arc::new(AtomicBool::new(false));
    let setter = {
        let flag = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            flag.store(true, Ordering::SeqCst);
        })
    };
    let started = std::time::Instant::now();
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        server
            .client()
            .send_stream("m", &[], 64, &[], &flag, |_| Box::pin(async {})),
    )
    .await
    .expect("the wait must end on the stop request, not on the Retry-After")
    .unwrap_err();
    setter.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the backoff was sat out: {:?}",
        started.elapsed()
    );
    assert!(
        format!("{err:#}").contains("aborted by user"),
        "the stop request, not the 429, is the verdict: {err:#}"
    );
    assert_eq!(
        server.attempts.load(Ordering::SeqCst),
        1,
        "no retry was made"
    );
}

// ---------------------------------------------- pre-output stream retries

/// The framing of a stream response this file scripts: the head, and
/// `Connection: close` so every attempt is its own connection.
const SSE_HEAD: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";

/// The role echo a stream opens with: empty content, nothing a caller sees.
const ROLE_ECHO: &str =
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n";

/// The callback `send_stream` takes: one boxed future per delta.
type OnDelta = Box<dyn FnMut(stream::Delta) -> Pin<Box<dyn Future<Output = ()> + Send>>>;

/// A complete streamed answer, as one raw response.
fn sse_response(text: &str) -> String {
    format!("{SSE_HEAD}{}", sse(text))
}

/// A 200 that ends after its role echo: no `[DONE]`, nothing assembled. The
/// shape of a server that died before the model produced anything.
fn empty_stream() -> String {
    format!("{SSE_HEAD}{ROLE_ECHO}")
}

/// A 200 whose body claims more bytes than arrive: the read fails with a
/// transport error instead of a clean EOF, as a reset connection would.
fn truncated(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 4096\r\n\
         Connection: close\r\n\r\n{body}"
    )
}

/// The deltas a stream delivered, shared with its callback.
type Deltas = Arc<Mutex<Vec<stream::Delta>>>;

/// A delta sink for a stream test: the shared list and the callback that
/// appends to it, so a test can prove nothing was delivered twice.
fn sink() -> (Deltas, OnDelta) {
    let deltas: Deltas = Arc::new(Mutex::new(Vec::new()));
    let shared = deltas.clone();
    let on_delta: OnDelta = Box::new(move |delta| {
        let sink = shared.clone();
        Box::pin(async move { sink.lock().unwrap().push(delta) })
    });
    (deltas, on_delta)
}

/// A 200 whose body ends before it delivers anything is worth another
/// attempt: nothing was shown, so nothing is repeated. The next stream
/// completes, its text arrives exactly once, and the retry is one notice.
#[tokio::test]
async fn an_empty_stream_is_retried_and_the_next_one_completes() {
    let server = Scripted::spawn(vec![empty_stream(), sse_response("streamed")]);
    let client = server.client();
    let (deltas, on_delta) = sink();
    let outcome = client
        .send_stream("m", &[], 64, &[], &never_abort(), on_delta)
        .await
        .unwrap();
    assert!(outcome.complete, "the second attempt reached [DONE]");
    assert_eq!(
        outcome.response.choices[0].message.content.as_deref(),
        Some("streamed")
    );
    assert_eq!(
        *deltas.lock().unwrap(),
        vec![stream::Delta::Text("streamed".into())],
        "nothing from the failed attempt, and the text once"
    );
    assert_eq!(server.attempts.load(Ordering::SeqCst), 2);
    let notices = client.take_notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("retry 1 of 5"), "{}", notices[0]);
}

/// The same for a read that fails outright before the first delta: the
/// wire's doing, retried like a dropped connection before the head.
#[tokio::test]
async fn a_read_error_before_the_first_delta_is_retried() {
    let server = Scripted::spawn(vec![
        truncated(": keep-alive\n\n"),
        sse_response("second try"),
    ]);
    let client = server.client();
    let (deltas, on_delta) = sink();
    let outcome = client
        .send_stream("m", &[], 64, &[], &never_abort(), on_delta)
        .await
        .unwrap();
    assert!(outcome.complete);
    assert_eq!(
        outcome.response.choices[0].message.content.as_deref(),
        Some("second try")
    );
    assert_eq!(
        *deltas.lock().unwrap(),
        vec![stream::Delta::Text("second try".into())]
    );
    assert_eq!(server.attempts.load(Ordering::SeqCst), 2);
    let notices = client.take_notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].contains("retry 1 of 5") && notices[0].contains("before any output"),
        "{}",
        notices[0]
    );
}

/// After a delta has gone out, the same failure stands as an error: the
/// partial was already shown, and another attempt would show it again.
#[tokio::test]
async fn a_read_error_after_a_delta_is_not_retried() {
    let server = Scripted::spawn(vec![
        truncated("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"half\"}}]}\n\n"),
        sse_response("never asked for"),
    ]);
    let client = server.client();
    let (deltas, on_delta) = sink();
    let err = client
        .send_stream("m", &[], 64, &[], &never_abort(), on_delta)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("reading SSE stream"), "{err:#}");
    assert_eq!(
        *deltas.lock().unwrap(),
        vec![stream::Delta::Text("half".into())],
        "the delta reached the caller exactly once"
    );
    assert_eq!(server.attempts.load(Ordering::SeqCst), 1, "no retry");
    assert!(client.take_notices().is_empty());
}

/// The budget is one for the whole call: a retry spent on the response
/// head is one the stream cannot have.
#[tokio::test]
async fn the_head_and_the_stream_share_one_budget() {
    let server = Scripted::spawn(vec![
        http("500 Internal Server Error", "", "nope"),
        empty_stream(),
        sse_response("never asked for"),
    ]);
    let client = server.client().with_retry(1);
    let err = client
        .send_stream("m", &[], 64, &[], &never_abort(), |_| Box::pin(async {}))
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("without [DONE]"),
        "the last failure is the stream's: {err:#}"
    );
    assert_eq!(
        server.attempts.load(Ordering::SeqCst),
        2,
        "the first attempt and one retry, whichever half needed it"
    );
    let notices = client.take_notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("retry 1 of 1"), "{}", notices[0]);
}

/// Empty every time: the retries stop at `max_retries`, each leaves a
/// notice, and the caller gets the last failure.
#[tokio::test]
async fn empty_streams_stop_at_the_budget() {
    let server = Scripted::spawn(vec![empty_stream(), empty_stream(), empty_stream()]);
    let client = server.client().with_retry(2);
    let err = client
        .send_stream("m", &[], 64, &[], &never_abort(), |_| Box::pin(async {}))
        .await
        .unwrap_err();
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("without [DONE]") && rendered.contains("assembled nothing"),
        "{rendered}"
    );
    assert_eq!(server.attempts.load(Ordering::SeqCst), 3);
    let notices = client.take_notices();
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(notices[1].contains("retry 2 of 2"), "{}", notices[1]);
}

/// A chunk that does not parse is the provider's answer to the request:
/// asked again it comes back the same, so there is no retry.
#[tokio::test]
async fn a_malformed_chunk_is_not_retried() {
    let server = Scripted::spawn(vec![
        format!("{SSE_HEAD}data: {{not json at all\n\ndata: [DONE]\n\n"),
        sse_response("never asked for"),
    ]);
    let client = server.client();
    let err = client
        .send_stream("m", &[], 64, &[], &never_abort(), |_| Box::pin(async {}))
        .await
        .unwrap_err();
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("parsing SSE chunk") && rendered.contains("{not json"),
        "{rendered}"
    );
    assert_eq!(server.attempts.load(Ordering::SeqCst), 1, "no retry");
    assert!(client.take_notices().is_empty());
}

/// The wait before a stream retry belongs to the stop request too: a stop
/// that lands in it ends the call as the user's, and the retry is never
/// made.
#[tokio::test]
async fn a_stop_request_ends_the_wait_before_a_stream_retry() {
    let server = Scripted::spawn(vec![empty_stream(), sse_response("never asked for")]);
    let client = LlmClient::new(&server.base_url)
        .unwrap()
        .with_retry_delay(Duration::from_secs(30));
    let flag = Arc::new(AtomicBool::new(false));
    let setter = {
        let flag = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            flag.store(true, Ordering::SeqCst);
        })
    };
    let started = std::time::Instant::now();
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        client.send_stream("m", &[], 64, &[], &flag, |_| Box::pin(async {})),
    )
    .await
    .expect("the wait must end on the stop request, not on the backoff")
    .unwrap_err();
    setter.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the backoff was sat out: {:?}",
        started.elapsed()
    );
    assert!(
        format!("{err:#}").contains("aborted by user"),
        "the stop request, not the empty stream, is the verdict: {err:#}"
    );
    assert_eq!(
        server.attempts.load(Ordering::SeqCst),
        1,
        "no retry was made"
    );
}
