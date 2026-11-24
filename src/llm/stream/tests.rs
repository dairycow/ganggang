//! Stream-assembly tests: pure line/assembly folds, plus end-to-end SSE
//! over a real socket through `LlmClient::send_stream`. Chunk sequences
//! mirror what Strata actually emits (probed live, see module docs).

use std::io::{Read, Write};
use std::net::TcpListener;

use serde_json::{Value, json};

use super::Assembler;
use crate::llm::client::LlmClient;
use crate::llm::stream::LineBuffer;

// ------------------------------------------------------------ pure pieces

/// Lines split across byte reads — including a multibyte char cut in half
/// and a CRLF terminator — reassemble exactly.
#[test]
fn line_buffer_reassembles_split_lines_and_multibyte_chars() {
    let mut buf = LineBuffer::new();
    // "h\u{e9}llo" with the two UTF-8 bytes of é split across feeds (the
    // \u escape keeps the test independent of source-file normalization),
    // CRLF ending.
    let e_acute = "\u{e9}";
    assert_eq!(e_acute.len(), 2, "str::len is bytes");
    let mut buf_bytes = Vec::new();
    buf_bytes.extend_from_slice(b"data: h");
    buf_bytes.extend_from_slice(&e_acute.as_bytes()[..1]);
    assert!(buf.feed(&buf_bytes).is_empty(), "partial line held back");
    let mut rest = Vec::new();
    rest.extend_from_slice(&e_acute.as_bytes()[1..]);
    rest.extend_from_slice(b"llo\r\n");
    assert_eq!(buf.feed(&rest), vec![format!("data: h{e_acute}llo")]);
    // Tail without a terminator stays pending, multiple lines in one read
    // all come out.
    assert_eq!(
        buf.feed(b"a\nb\ndata: tail"),
        vec!["a".to_string(), "b".to_string()]
    );
    assert_eq!(buf.feed(b"\n"), vec!["data: tail".to_string()]);
}

/// The Assembler reproduces the exact `ChatResponse` of the equivalent
/// non-streaming reply, and yields classified deltas in arrival order —
/// thinking first, then text, exactly the wire order. The chunk sequence
/// is Strata's real one: keep-alive handled by the caller, role echo
/// with empty content, reasoning fragments, content pieces, empty-delta
/// finish chunk, usage chunk, `[DONE]`.
#[test]
fn assembles_a_text_stream_into_the_non_streaming_response() {
    let lines = [
        r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"reasoning_content":"We need"},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"reasoning_content":" to answer"},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"4"},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"2"},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"timings":{"predicted_n":41}}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":66,"completion_tokens":41,"total_tokens":107}}"#,
    ];
    let mut asm = Assembler::default();
    let mut deltas = Vec::new();
    for line in lines {
        for delta in asm.feed(serde_json::from_str(line).unwrap()) {
            deltas.push(delta);
        }
    }

    use super::Delta;
    assert_eq!(
        deltas,
        vec![
            Delta::Thinking("We need".into()),
            Delta::Thinking(" to answer".into()),
            Delta::Text("4".into()),
            Delta::Text("2".into()),
        ]
    );

    let expected: crate::llm::types::ChatResponse = serde_json::from_value(json!({
        "choices": [{
            "message": {"role": "assistant", "content": "42", "reasoning_content": "We need to answer"},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 66, "completion_tokens": 41, "total_tokens": 107},
        "timings": {"predicted_n": 41},
    }))
    .unwrap();
    assert_eq!(asm.finish(), expected);
}

/// Tool-call fragments — id+name on the first, arguments split mid-JSON,
/// Strata's own split points — assemble into the tool-call response shape
/// (content `None`, `finish_reason: "tool_calls"`).
#[test]
fn assembles_tool_call_fragments_split_mid_json() {
    let lines = [
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_df5a","type":"function","function":{"name":"write","arguments":""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"path\":\""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"hello.txt\",\"content\":\"hi\""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"}"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":322,"completion_tokens":50,"total_tokens":372}}"#,
    ];
    let mut asm = Assembler::default();
    for line in lines {
        asm.feed(serde_json::from_str(line).unwrap());
    }

    let resp = asm.finish();
    assert_eq!(resp.usage.unwrap().total_tokens, 372);
    let choice = &resp.choices[0];
    assert_eq!(choice.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(choice.message.content, None);
    let call = &choice.message.tool_calls[0];
    assert_eq!(call.id, "call_df5a");
    assert_eq!(call.function.name, "write");
    assert_eq!(
        call.function.arguments,
        r#"{"path":"hello.txt","content":"hi"}"#
    );
}

/// Two tool calls interleaved by index still come out in index order,
/// each with its own id/name/arguments.
#[test]
fn interleaved_call_fragments_assemble_in_index_order() {
    let lines = [
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"echo","arguments":""}},{"index":1,"id":"call_b","function":{"name":"read","arguments":""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"path\""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"text\":\"1\"}"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":":\"x\"}"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
    ];
    let mut asm = Assembler::default();
    for line in lines {
        asm.feed(serde_json::from_str(line).unwrap());
    }
    let calls = &asm.finish().choices[0].message.tool_calls;
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, "call_a");
    assert_eq!(calls[0].function.arguments, r#"{"text":"1"}"#);
    assert_eq!(calls[1].id, "call_b");
    assert_eq!(calls[1].function.arguments, r#"{"path":"x"}"#);
}

/// The role-echo chunk's `content: ""` is not content: a tool-call turn
/// that opened with it must still finish with `content: None`, matching
/// the non-streaming wire shape exactly.
#[test]
fn role_echo_empty_content_yields_none_alongside_tool_calls() {
    let lines = [
        r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_df5a","function":{"name":"read","arguments":"{}"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
    ];
    let mut asm = Assembler::default();
    for line in lines {
        asm.feed(serde_json::from_str(line).unwrap());
    }
    let resp = asm.finish();
    assert_eq!(resp.choices[0].message.content, None);
    assert_eq!(resp.choices[0].message.tool_calls.len(), 1);
}

/// `id`/`name` on continuation fragments: a non-empty repeat overwrites,
/// an empty or absent one leaves the first fragment's value — providers
/// differ on whether they re-send the header, and either way the call
/// must come out identified.
#[test]
fn continuation_fragments_carry_id_and_name_semantics() {
    let lines = [
        // First fragment establishes id and name.
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_first","function":{"name":"bash","arguments":"{\"command\":"}}]},"finish_reason":null}]}"#,
        // Continuation repeats name (overwrites, same value in practice)
        // but no id; arguments continue.
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"","function":{"name":"bash","arguments":"\"echo\""}}]},"finish_reason":null}]}"#,
        // A later fragment with a NEW non-empty id wins (last speaker
        // with a value is authoritative).
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_corrected","function":{"arguments":"}"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
    ];
    let mut asm = Assembler::default();
    for line in lines {
        asm.feed(serde_json::from_str(line).unwrap());
    }
    let call = &asm.finish().choices[0].message.tool_calls[0];
    assert_eq!(call.id, "call_corrected");
    assert_eq!(call.function.name, "bash");
    assert_eq!(call.function.arguments, r#"{"command":"echo"}"#);
}

// ------------------------------------------------- end-to-end over a socket

/// Serve one SSE body per connection, in several small writes so reads
/// arrive as multiple chunks (line splits are exercised deterministically
/// by the pure tests; this covers HTTP + chunk framing end to end).
struct SseServer {
    base_url: String,
}

impl SseServer {
    fn spawn(bodies: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for body in bodies {
                let (mut stream, _) = listener.accept().unwrap();
                read_request_head(&mut stream);
                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
                stream.write_all(head.as_bytes()).unwrap();
                // Fragmented on purpose: never aligned with line ends.
                for piece in body.as_bytes().chunks(7) {
                    stream.write_all(piece).unwrap();
                }
                drop(stream);
            }
        });
        Self {
            base_url: format!("http://{addr}"),
        }
    }
}

fn read_request_head(stream: &mut std::net::TcpStream) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "connection closed before headers finished");
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn sse_body(events: &[Value]) -> String {
    let mut out = String::from(": keep-alive\n\n");
    for event in events {
        out.push_str(&format!("data: {event}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
    out
}

#[tokio::test]
async fn send_stream_assembles_and_delivers_deltas_in_order() {
    let body = sse_body(&[
        json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"content":"g"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"content":"g!"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":3,"total_tokens":12}}),
    ]);
    let server = SseServer::spawn(vec![body]);

    let client = LlmClient::new(&server.base_url).unwrap();
    // The callback future is 'static, so it cannot borrow the test's Vec —
    // collect through shared state instead.
    let deltas = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = deltas.clone();
    let response = client
        .send_stream("m", &[], 64, &[], move |delta| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().unwrap().push(delta);
            })
        })
        .await
        .unwrap();

    use super::Delta;
    assert_eq!(
        *deltas.lock().unwrap(),
        vec![Delta::Text("g".into()), Delta::Text("g!".into())]
    );
    assert_eq!(response.choices[0].message.content.as_deref(), Some("gg!"));
    assert_eq!(response.choices[0].finish_reason.as_deref(), Some("stop"));
    assert_eq!(response.usage.unwrap().total_tokens, 12);
}

/// A stream that ends without `[DONE]` is a transport error, never a
/// silently-accepted partial answer (§17) — Strata's SSE arrives
/// connection-close framed, so a mid-generation death is a clean EOF.
#[tokio::test]
async fn truncated_stream_without_done_is_an_error() {
    // Same body as the happy path, minus the `[DONE]` terminator; the
    // server closing the connection plays the part of a dead upstream.
    let mut body = sse_body(&[
        json!({"choices":[{"index":0,"delta":{"content":"par"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
    ]);
    assert!(body.ends_with("data: [DONE]\n\n"));
    body.truncate(body.len() - "data: [DONE]\n\n".len());
    let server = SseServer::spawn(vec![body]);

    let client = LlmClient::new(&server.base_url).unwrap();
    let err = client
        .send_stream("m", &[], 64, &[], |_| Box::pin(async {}))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("without [DONE]"),
        "unexpected error: {err:#}"
    );
}

/// A `data:` line whose JSON does not parse aborts with context naming
/// the offending chunk — a provider-shape oddity must not be swallowed
/// into whatever assembled before it (§17: visible, never silent).
#[tokio::test]
async fn malformed_data_chunk_mid_stream_aborts_with_context() {
    // Built by hand so the malformed line sits BEFORE the [DONE]
    // sentinel the reader would otherwise stop at.
    let mut body = String::from(
        ": keep-alive\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"so far so\"}}]}\n\n",
    );
    body.push_str("data: {not json at all\n\ndata: [DONE]\n\n");
    let server = SseServer::spawn(vec![body]);

    let client = LlmClient::new(&server.base_url).unwrap();
    let err = client
        .send_stream("m", &[], 64, &[], |_| Box::pin(async {}))
        .await
        .unwrap_err();
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("parsing SSE chunk") && rendered.contains("{not json"),
        "unexpected error: {rendered}"
    );
}

#[test]
fn delta_reasoning_fallback_populates_thinking_and_reasoning_content() {
    let lines = [
        r#"{"choices":[{"index":0,"delta":{"reasoning":"step 1"},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
    ];
    let mut asm = Assembler::default();
    let mut deltas = Vec::new();
    for line in lines {
        deltas.extend(asm.feed(serde_json::from_str(line).unwrap()));
    }
    assert_eq!(
        deltas,
        vec![
            super::Delta::Thinking("step 1".into()),
            super::Delta::Text("ok".into())
        ]
    );
    let resp = asm.finish();
    assert_eq!(
        resp.choices[0].message.reasoning_content.as_deref(),
        Some("step 1")
    );
}

#[test]
fn assembles_full_strata_timings_and_usage() {
    let lines = [
        r#"{"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"timings":{"cache_n":0,"prompt_n":42,"prompt_ms":1653.5,"prompt_per_second":25.4,"predicted_n":22,"predicted_ms":520.2,"predicted_per_second":42.3}}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":42,"completion_tokens":22,"total_tokens":64}}"#,
    ];
    let mut asm = Assembler::default();
    for line in lines {
        asm.feed(serde_json::from_str(line).unwrap());
    }
    let resp = asm.finish();
    let timings = resp.timings.expect("timings captured");
    assert_eq!(timings.decode_tok_per_sec(), Some(42.3));
    assert_eq!(timings.prompt_tok_per_sec(), Some(25.4));
    assert_eq!(resp.usage.unwrap().total_tokens, 64);
}
