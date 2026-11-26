//! Pre-output stream retries, Anthropic flavour: the same contract as the
//! OpenAI stream (`client::stream_tests`), over `message_stop`.

use std::time::Duration;

use super::*;

/// A stream that opens and stops before any content: `message_start` and
/// a ping, no `message_stop`. The shape of a server that died before the
/// model produced anything.
fn empty_stream() -> String {
    format!(
        "{SSE_HEAD}event: message_start\n\
         data: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":5,\"output_tokens\":0}}}}}}\n\n\
         data: {{\"type\":\"ping\"}}\n\n"
    )
}

/// One text delta, then `message_stop`.
fn complete_stream(text: &str) -> String {
    format!(
        "{SSE_HEAD}event: content_block_delta\n\
         data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{text}\"}}}}\n\n\
         event: message_stop\n\
         data: {{\"type\":\"message_stop\"}}\n\n"
    )
}

/// A 200 whose body claims more bytes than arrive: the read fails with a
/// transport error instead of a clean EOF, as a reset connection would.
fn truncated(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 4096\r\n\
         Connection: close\r\n\r\n{body}"
    )
}

fn client(base_url: &str) -> LlmClient {
    LlmClient::new(base_url)
        .unwrap()
        .with_api(ApiKind::AnthropicMessages)
        .with_retry_delay(Duration::ZERO)
}

/// A stream that ends before it delivers anything is worth another
/// attempt: the next one completes, and the retry is one notice.
#[tokio::test]
async fn an_empty_anthropic_stream_is_retried_and_the_next_one_completes() {
    let server = CutServer::spawn_raw(vec![empty_stream(), complete_stream("second try")]);
    let client = client(&server.base_url);
    let outcome = client
        .send_stream("claude-haiku-5-5", &[], 64, &[], &never_abort(), |_| {
            Box::pin(async {})
        })
        .await
        .unwrap();
    assert!(outcome.complete, "message_stop was seen on the retry");
    assert_eq!(
        outcome.response.choices[0].message.content.as_deref(),
        Some("second try")
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

/// The same for a read that fails outright before the first delta.
#[tokio::test]
async fn an_anthropic_read_error_before_the_first_delta_is_retried() {
    let server = CutServer::spawn_raw(vec![
        truncated("data: {\"type\":\"ping\"}\n\n"),
        complete_stream("second try"),
    ]);
    let client = client(&server.base_url);
    let outcome = client
        .send_stream("claude-haiku-5-5", &[], 64, &[], &never_abort(), |_| {
            Box::pin(async {})
        })
        .await
        .unwrap();
    assert!(outcome.complete);
    assert_eq!(
        outcome.response.choices[0].message.content.as_deref(),
        Some("second try")
    );
    assert_eq!(server.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(client.take_notices().len(), 1);
}

/// A cut after a delta keeps its partial and makes no second attempt: the
/// text was already shown, and a replay would show it again.
#[tokio::test]
async fn an_anthropic_cut_after_a_delta_is_salvaged_and_not_retried() {
    let cut = concat!(
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"half an\"}}\n\n",
    );
    let server = CutServer::spawn_raw(vec![
        format!("{SSE_HEAD}{cut}"),
        complete_stream("never asked for"),
    ]);
    let client = client(&server.base_url);
    let outcome = client
        .send_stream("claude-haiku-5-5", &[], 64, &[], &never_abort(), |_| {
            Box::pin(async {})
        })
        .await
        .unwrap();
    assert!(!outcome.complete, "no message_stop, so not complete");
    assert_eq!(
        outcome.response.choices[0].message.content.as_deref(),
        Some("half an")
    );
    assert_eq!(server.attempts.load(Ordering::SeqCst), 1, "no retry");
    assert!(client.take_notices().is_empty());
}

/// A provider `error` event is the server's answer to the request, sent
/// over a 200: asked again it comes back the same, so there is no retry.
#[tokio::test]
async fn an_anthropic_error_event_is_not_retried() {
    let event = "data: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit\",\"message\":\"slow down\"}}\n\n";
    let server = CutServer::spawn_raw(vec![
        format!("{SSE_HEAD}{event}"),
        complete_stream("never asked for"),
    ]);
    let client = client(&server.base_url);
    let err = client
        .send_stream("claude-haiku-5-5", &[], 64, &[], &never_abort(), |_| {
            Box::pin(async {})
        })
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("slow down"), "{err:#}");
    assert_eq!(server.attempts.load(Ordering::SeqCst), 1, "no retry");
    assert!(client.take_notices().is_empty());
}

/// Nor is a chunk that does not parse.
#[tokio::test]
async fn a_malformed_anthropic_chunk_is_not_retried() {
    let server = CutServer::spawn_raw(vec![
        format!("{SSE_HEAD}data: {{not json at all\n\n"),
        complete_stream("never asked for"),
    ]);
    let client = client(&server.base_url);
    let err = client
        .send_stream("claude-haiku-5-5", &[], 64, &[], &never_abort(), |_| {
            Box::pin(async {})
        })
        .await
        .unwrap_err();
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("parsing Anthropic SSE chunk") && rendered.contains("{not json"),
        "{rendered}"
    );
    assert_eq!(server.attempts.load(Ordering::SeqCst), 1, "no retry");
    assert!(client.take_notices().is_empty());
}
