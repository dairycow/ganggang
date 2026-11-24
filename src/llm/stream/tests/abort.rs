//! Esc mid-request (the request half of GAP 3): the SSE read watches the
//! run's stop request, so a generation ends when the user asks instead of
//! when the wire decides. What streamed in is kept; an abort that lands
//! before the first token keeps nothing and is still not an error.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;

use super::*;

/// Serves the response head plus one SSE fragment, then holds the
/// connection open — the shape of a generation still running. Nothing can
/// end the read but the abort flag, which is what an abort test needs: a
/// dropped connection would be EOF, and EOF is the salvage path.
struct StallingSseServer {
    base_url: String,
}

impl StallingSseServer {
    fn spawn(fragment: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request_head(&mut stream);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            stream.write_all(fragment.as_bytes()).unwrap();
            // Stay silent well past any deadline the test could want;
            // leaving the connection open is the point.
            std::thread::sleep(std::time::Duration::from_secs(30));
            drop(stream);
        });
        Self {
            base_url: format!("http://{addr}"),
        }
    }
}

/// Esc in the middle of a generation: the read stops at its next
/// poll instead of waiting for a stream that has minutes left to run, and
/// the text the user already watched arrive is kept.
#[tokio::test]
async fn aborting_mid_stream_keeps_the_text_that_arrived() {
    let fragment = format!(
        "data: {}\n\n",
        json!({"choices":[{"index":0,"delta":{"content":"half an"}}]})
    );
    let server = StallingSseServer::spawn(fragment);

    let abort = Arc::new(AtomicBool::new(false));
    let flag = abort.clone();
    let setter = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        flag.store(true, Ordering::Relaxed);
    });

    let started = std::time::Instant::now();
    let outcome = LlmClient::new(&server.base_url)
        .unwrap()
        .with_retry(0)
        .send_stream("m", &[], 64, &[], &abort, |_| Box::pin(async {}))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    setter.await.unwrap();

    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "the stop request must cut the read short, not the timeout: {elapsed:?}"
    );
    assert!(outcome.aborted, "the user stopped this stream");
    assert!(!outcome.complete, "no [DONE] was seen");
    assert_eq!(
        outcome.response.choices[0].message.content.as_deref(),
        Some("half an"),
        "the partial answer survives an abort"
    );
}

/// A stop request that lands before a single token arrives is still not an
/// error: there is simply nothing to keep. `complete: false` with an empty
/// response and `aborted: true` is the loop's cue to end the run, not to
/// salvage-continue from nothing.
#[tokio::test]
async fn aborting_before_any_text_returns_an_empty_partial() {
    let server = StallingSseServer::spawn(String::new());

    let abort = Arc::new(AtomicBool::new(false));
    let flag = abort.clone();
    let setter = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        flag.store(true, Ordering::Relaxed);
    });

    let started = std::time::Instant::now();
    let outcome = LlmClient::new(&server.base_url)
        .unwrap()
        .with_retry(0)
        .send_stream("m", &[], 64, &[], &abort, |_| Box::pin(async {}))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    setter.await.unwrap();

    assert!(elapsed < std::time::Duration::from_secs(2), "{elapsed:?}");
    assert!(outcome.aborted);
    assert!(!outcome.complete);
    assert_eq!(
        outcome.response.choices[0].message.content, None,
        "nothing assembled"
    );
}
