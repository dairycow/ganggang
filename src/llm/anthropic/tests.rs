mod abort;
mod retry;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::stream::AnthropicAssembler;
use super::{build_request, messages_url, parse_response};
use crate::llm::client::LlmClient;
use crate::llm::stream::Delta;
use crate::llm::types::{ChatMessage, ToolCall};
use crate::llm::{ApiKind, AuthStyle};

#[test]
fn messages_url_handles_with_and_without_v1() {
    assert_eq!(
        messages_url("https://opencode.ai/inference/go/anthropic"),
        "https://opencode.ai/inference/go/anthropic/v1/messages"
    );
    assert_eq!(
        messages_url("https://opencode.ai/inference/go/anthropic/v1/"),
        "https://opencode.ai/inference/go/anthropic/v1/messages"
    );
    assert_eq!(
        messages_url("https://api.z.ai/api/anthropic"),
        "https://api.z.ai/api/anthropic/v1/messages"
    );
}

#[test]
fn build_request_extracts_system_converts_tools_and_merges_user_turns() {
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("sys 1".into()),
            ..Default::default()
        },
        ChatMessage {
            role: "user".into(),
            content: Some("[compacted]".into()),
            ..Default::default()
        },
        ChatMessage {
            role: "user".into(),
            content: Some("next question".into()),
            ..Default::default()
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: vec![
                ToolCall::function("c1".into(), "read".into(), r#"{"path":"a"}"#.into()),
                ToolCall::function("c2".into(), "read".into(), r#"{"path":"b"}"#.into()),
            ],
            ..Default::default()
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("contents a".into()),
            tool_call_id: Some("c1".into()),
            ..Default::default()
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("missing b".into()),
            tool_call_id: Some("c2".into()),
            is_error: true,
            ..Default::default()
        },
    ];
    let tools = vec![json!({
        "type": "function",
        "function": {
            "name": "read",
            "description": "read a file",
            "parameters": { "type": "object", "properties": { "path": { "type": "string" } } }
        }
    })];

    let req = build_request("claude-haiku-5-5", &messages, 4096, &tools, true);
    assert_eq!(req["model"], "claude-haiku-5-5");
    assert_eq!(req["max_tokens"], 4096);
    assert_eq!(req["system"], "sys 1");
    assert_eq!(req["stream"], true);
    assert_eq!(req["tools"][0]["name"], "read");
    assert_eq!(
        req["tools"][0]["input_schema"]["properties"]["path"]["type"],
        "string"
    );

    let msgs = req["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3, "merged user, assistant, merged tool_results");
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"].as_array().unwrap().len(), 2);
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["content"][0]["type"], "tool_use");
    assert_eq!(msgs[1]["content"][0]["input"]["path"], "a");
    assert_eq!(msgs[2]["role"], "user");
    assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    assert_eq!(msgs[2]["content"][1]["is_error"], true);
}

#[test]
fn parse_response_maps_blocks_usage_and_stop_reason() {
    let raw = json!({
        "content": [
            { "type": "thinking", "thinking": "hmm" },
            { "type": "text", "text": "hello" },
            { "type": "tool_use", "id": "tu_1", "name": "read", "input": { "path": "Cargo.toml" } }
        ],
        "stop_reason": "tool_use",
        "usage": {
            "input_tokens": 10,
            "cache_read_input_tokens": 20,
            "cache_creation_input_tokens": 5,
            "output_tokens": 7
        }
    });
    let resp = parse_response(raw).unwrap();
    assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("tool_calls"));
    let msg = &resp.choices[0].message;
    assert_eq!(msg.reasoning_content.as_deref(), Some("hmm"));
    assert_eq!(msg.content.as_deref(), Some("hello"));
    assert_eq!(msg.tool_calls[0].id, "tu_1");
    assert_eq!(msg.tool_calls[0].function.name, "read");
    assert!(msg.tool_calls[0].function.arguments.contains("Cargo.toml"));
    let u = resp.usage.unwrap();
    assert_eq!(
        (u.prompt_tokens, u.completion_tokens, u.total_tokens),
        (35, 7, 42)
    );
    // The cache reads stay separated out: prompt is the full request,
    // cache_read is the part of it served from the prompt cache.
    assert_eq!(u.cache_read_tokens, 20);
}

#[test]
fn assembler_folds_thinking_text_and_split_tool_json() {
    let events = [
        json!({"type":"message_start","message":{"usage":{"input_tokens":12,"output_tokens":0,"cache_read_input_tokens":8}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"plan"}}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"hi"}}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"tu_9","name":"bash","input":{}}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"command\":"}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"ls\"}"}}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}),
    ];
    let mut asm = AnthropicAssembler::default();
    let mut deltas = Vec::new();
    for ev in &events {
        deltas.extend(asm.feed(None, ev).unwrap());
    }
    assert_eq!(
        deltas,
        vec![Delta::Thinking("plan".into()), Delta::Text("hi".into()),]
    );
    let resp = asm.finish();
    assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(
        resp.choices[0].message.reasoning_content.as_deref(),
        Some("plan")
    );
    assert_eq!(resp.choices[0].message.content.as_deref(), Some("hi"));
    assert_eq!(
        resp.choices[0].message.tool_calls[0].function.arguments,
        r#"{"command":"ls"}"#
    );
    // 12 fresh input plus 8 cache reads, plus 9 output.
    let u = resp.usage.unwrap();
    assert_eq!(
        (u.prompt_tokens, u.completion_tokens, u.total_tokens),
        (20, 9, 29)
    );
    assert_eq!(u.cache_read_tokens, 8);
}

/// Headers and the three ways a stream can go wrong: the truncated one
/// fails on its first attempt here (retries are off, so the script stays
/// three connections long; `tests::retry` covers the retried case).
#[tokio::test]
async fn anthropic_client_sends_headers_and_handles_sse_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen_headers = Arc::new(Mutex::new(String::new()));
    let seen_clone = seen_headers.clone();

    std::thread::spawn(move || {
        // 1. Happy stream
        let (mut s, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let n = s.read(&mut buf).unwrap();
        *seen_clone.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}"
        );
        s.write_all(resp.as_bytes()).unwrap();
        drop(s);

        // 2. Truncated stream (no message_stop, nothing assembled)
        let (mut s, _) = listener.accept().unwrap();
        let _ = s.read(&mut buf);
        let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"ping\"}\n\n";
        s.write_all(resp.as_bytes()).unwrap();
        drop(s);

        // 3. Error event
        let (mut s, _) = listener.accept().unwrap();
        let _ = s.read(&mut buf);
        let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit\",\"message\":\"slow down\"}}\n\n";
        s.write_all(resp.as_bytes()).unwrap();
    });

    let client = LlmClient::new(&format!("http://{addr}"))
        .unwrap()
        .with_api(ApiKind::AnthropicMessages)
        .with_auth(AuthStyle::Auto)
        .with_api_key(Some("secret-k".into()))
        .with_retry(0);

    let outcome = client
        .send_stream("claude-haiku-5-5", &[], 64, &[], &never_abort(), |_| {
            Box::pin(async {})
        })
        .await
        .unwrap();
    assert!(outcome.complete, "message_stop was seen");
    assert_eq!(
        outcome.response.choices[0].message.content.as_deref(),
        Some("ok")
    );
    let raw_req = seen_headers.lock().unwrap().to_ascii_lowercase();
    assert!(raw_req.contains("post /v1/messages "), "{raw_req}");
    assert!(raw_req.contains("x-api-key: secret-k"), "{raw_req}");
    assert!(
        raw_req.contains("anthropic-version: 2023-06-01"),
        "{raw_req}"
    );

    let err_eof = client
        .send_stream("claude-haiku-5-5", &[], 64, &[], &never_abort(), |_| {
            Box::pin(async {})
        })
        .await
        .unwrap_err();
    assert!(
        err_eof.to_string().contains("without message_stop"),
        "{err_eof:#}"
    );

    let err_ev = client
        .send_stream("claude-haiku-5-5", &[], 64, &[], &never_abort(), |_| {
            Box::pin(async {})
        })
        .await
        .unwrap_err();
    assert!(err_ev.to_string().contains("slow down"), "{err_ev:#}");
}

/// The same salvage rule as the OpenAI stream (GAP 2): a body cut before
/// `message_stop` keeps what it assembled and reports `complete: false`
/// with the usage seen so far; a cut that assembled nothing still fails
/// (retries are off here, so it fails on its first attempt).
#[tokio::test]
async fn an_anthropic_stream_cut_mid_text_is_salvaged() {
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":4,\"output_tokens\":0}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"half an\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"swer\"}}\n\n",
    );
    let server = CutServer::spawn(vec![
        body.to_string(),
        "data: {\"type\":\"ping\"}\n\n".to_string(),
    ]);
    let client = LlmClient::new(&server.base_url)
        .unwrap()
        .with_api(ApiKind::AnthropicMessages)
        .with_retry(0);

    let outcome = client
        .send_stream("claude-haiku-5-5", &[], 64, &[], &never_abort(), |_| {
            Box::pin(async {})
        })
        .await
        .unwrap();
    assert!(!outcome.complete, "no message_stop, so not complete");
    assert!(!outcome.aborted, "the wire cut it, the user did not");
    assert_eq!(
        outcome.response.choices[0].message.content.as_deref(),
        Some("half answer"),
        "the text assembled before the cut survives"
    );
    assert_eq!(
        outcome.response.usage.unwrap().prompt_tokens,
        4,
        "usage seen before the cut is kept"
    );

    let err = client
        .send_stream("claude-haiku-5-5", &[], 64, &[], &never_abort(), |_| {
            Box::pin(async {})
        })
        .await
        .unwrap_err();
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("without message_stop") && rendered.contains("assembled nothing"),
        "unexpected error: {rendered}"
    );
}

/// Serves one response per connection, then closes: a clean EOF standing
/// in for a server that died mid-generation. Connections are counted, so a
/// script can assert how many attempts a call made.
struct CutServer {
    base_url: String,
    attempts: Arc<AtomicUsize>,
}

impl CutServer {
    /// One SSE body per connection, with the stream's response head in
    /// front of it.
    fn spawn(bodies: Vec<String>) -> Self {
        let responses = bodies
            .into_iter()
            .map(|body| format!("{SSE_HEAD}{body}"))
            .collect();
        Self::serve(responses)
    }

    /// Whole raw HTTP responses, exactly as given: for scripts about the
    /// transport, where the framing is the point.
    fn spawn_raw(responses: Vec<String>) -> Self {
        Self::serve(responses)
    }

    fn serve(responses: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let counted = attempts.clone();
        std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                read_anthropic_request_head(&mut stream);
                counted.fetch_add(1, Ordering::SeqCst);
                for piece in response.as_bytes().chunks(7) {
                    stream.write_all(piece).unwrap();
                }
                drop(stream);
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            attempts,
        }
    }
}

/// The framing of a stream response this module scripts: the head, and
/// `Connection: close` so every attempt is its own connection.
const SSE_HEAD: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";

fn read_anthropic_request_head(stream: &mut std::net::TcpStream) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "connection closed before headers finished");
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// The flag for a run nobody stopped: a stream read polls it, so every
/// script that is not about aborting hands it a flag that stays clear.
fn never_abort() -> AtomicBool {
    AtomicBool::new(false)
}
