//! Streaming client tests: the request half of a stream retries like any
//! request, the body half never does, and a body cut mid-answer is
//! salvage rather than loss (GAP 2).

use std::io::Write as _;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use super::LlmClient;
use super::stream;
use super::tests::{Scripted, http, read_request_head, sse};

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
        .send_stream("m", &[], 64, &[], |_| Box::pin(async {}))
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
        .send_stream("m", &[], 64, &[], move |delta| {
            let sink = sink.clone();
            Box::pin(async move { sink.lock().unwrap().push(delta) })
        })
        .await
        .unwrap();
    assert!(!outcome.complete, "no [DONE], so not complete");
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
        .send_stream("m", &[], 64, &[], |_| Box::pin(async {}))
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(4), "{elapsed:?}");
    assert!(err.to_string().contains("reading SSE stream"), "{err:#}");
}
