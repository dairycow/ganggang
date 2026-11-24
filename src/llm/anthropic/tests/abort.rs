//! Esc mid-request, Anthropic flavour: the drain polls the run's stop
//! request, so a stopped generation keeps what streamed in and ends at the
//! next poll instead of at the request timeout — and a transport cut still
//! reports itself as the wire's fault, not the user's.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

/// Serves the head plus one SSE fragment, then holds the connection open:
/// only the abort flag can end the read, which is what an abort script
/// needs (a closed connection would be EOF, i.e. the salvage path).
struct StallingServer {
    base_url: String,
}

impl StallingServer {
    fn spawn(fragment: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_anthropic_request_head(&mut stream);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            stream.write_all(fragment.as_bytes()).unwrap();
            std::thread::sleep(std::time::Duration::from_secs(30));
            drop(stream);
        });
        Self {
            base_url: format!("http://{addr}"),
        }
    }
}

/// The Anthropic drain honors the run's stop request like the OpenAI one
/// (the in-flight half of GAP 3): the partial the user already watched is kept, and the read
/// ends at its next poll rather than at the request timeout.
#[tokio::test]
async fn aborting_an_anthropic_stream_keeps_its_partial() {
    let fragment = concat!(
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"half an\"}}\n\n",
    );
    let server = StallingServer::spawn(fragment.to_string());
    let client = LlmClient::new(&server.base_url)
        .unwrap()
        .with_api(ApiKind::AnthropicMessages)
        .with_retry(0);

    let abort = Arc::new(AtomicBool::new(false));
    let flag = abort.clone();
    let setter = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        flag.store(true, Ordering::Relaxed);
    });

    let started = std::time::Instant::now();
    let outcome = client
        .send_stream("claude-haiku-5-5", &[], 64, &[], &abort, |_| {
            Box::pin(async {})
        })
        .await
        .unwrap();
    let elapsed = started.elapsed();
    setter.await.unwrap();

    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "the stop request must end the read: {elapsed:?}"
    );
    assert!(outcome.aborted, "the user stopped this stream");
    assert!(!outcome.complete, "no message_stop was seen");
    assert_eq!(
        outcome.response.choices[0].message.content.as_deref(),
        Some("half an"),
        "the text streamed so far survives"
    );

    // And an abort that lands before any fragment at all: an empty partial,
    // not the "assembled nothing" error the salvage path reserves for a cut.
    let server = StallingServer::spawn(String::new());
    let client = LlmClient::new(&server.base_url)
        .unwrap()
        .with_api(ApiKind::AnthropicMessages)
        .with_retry(0);
    let abort = Arc::new(AtomicBool::new(false));
    let flag = abort.clone();
    let setter = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        flag.store(true, Ordering::Relaxed);
    });
    let outcome = client
        .send_stream("claude-haiku-5-5", &[], 64, &[], &abort, |_| {
            Box::pin(async {})
        })
        .await
        .unwrap();
    setter.await.unwrap();
    assert!(outcome.aborted && !outcome.complete);
    assert_eq!(outcome.response.choices[0].message.content, None);
}

/// A transport cut is still the wire's fault, not the user's: the partial
/// comes back `aborted: false` so the loop continues the answer.
#[tokio::test]
async fn an_anthropic_cut_reports_itself_as_not_aborted() {
    let body = concat!(
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"salvaged\"}}\n\n",
    );
    let server = CutServer::spawn(vec![body.to_string()]);
    let outcome = LlmClient::new(&server.base_url)
        .unwrap()
        .with_api(ApiKind::AnthropicMessages)
        .send_stream("claude-haiku-5-5", &[], 64, &[], &never_abort(), |_| {
            Box::pin(async {})
        })
        .await
        .unwrap();
    assert!(!outcome.complete);
    assert!(!outcome.aborted, "the wire cut it, not the user");
    assert_eq!(
        outcome.response.choices[0].message.content.as_deref(),
        Some("salvaged")
    );
}
