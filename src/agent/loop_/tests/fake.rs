//! A scripted OpenAI-compatible server on a std `TcpListener`: serves
//! canned responses in sequence and records every request body it saw,
//! so replay can be asserted, not assumed.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

pub(super) struct FakeServer {
    pub(super) base_url: String,
    pub(super) requests: Arc<Mutex<Vec<Value>>>,
}

impl FakeServer {
    /// Serves each response to one accepted connection, in order, then
    /// stops. `Connection: close` makes every request a fresh connection,
    /// so mapping responses to requests is positional.
    pub(super) fn spawn(responses: Vec<String>) -> Self {
        serve(responses, "application/json", false)
    }

    /// The streaming scripted model: bodies are SSE streams served as
    /// `text/event-stream`, written in fragments so the client's chunk
    /// and line buffering face real splits.
    pub(super) fn spawn_sse(streams: Vec<String>) -> Self {
        serve(streams, "text/event-stream", true)
    }

    /// [`spawn`](Self::spawn), plus a hook called with each request's
    /// 0-based index after it is read and before the answer is written —
    /// the seam a script uses to change harness state mid-run, e.g. a
    /// user pressing Esc while a request is in flight.
    pub(super) fn spawn_with_hook(
        responses: Vec<String>,
        on_request: impl Fn(usize) + Send + 'static,
    ) -> Self {
        serve_hooked(responses, "application/json", false, on_request)
    }

    /// Answers with whole raw HTTP responses, exactly as given (status
    /// line, headers, body) — for scripts about the transport itself, a
    /// retried 429 say, where the status is the point.
    pub(super) fn spawn_raw(responses: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request_body(&mut stream);
                seen.lock()
                    .unwrap()
                    .push(serde_json::from_str(&request).unwrap());
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        FakeServer {
            base_url: format!("http://{addr}"),
            requests,
        }
    }
}

/// The accept loop both spawners share: one body per connection,
/// requests recorded in order.
fn serve(bodies: Vec<String>, content_type: &str, fragmented: bool) -> FakeServer {
    serve_hooked(bodies, content_type, fragmented, |_| {})
}

fn serve_hooked(
    bodies: Vec<String>,
    content_type: &str,
    fragmented: bool,
    on_request: impl Fn(usize) + Send + 'static,
) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let content_type = content_type.to_string();
    thread::spawn(move || {
        for (index, body) in bodies.into_iter().enumerate() {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request_body(&mut stream);
            seen.lock()
                .unwrap()
                .push(serde_json::from_str(&request).unwrap());
            on_request(index);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(head.as_bytes()).unwrap();
            if fragmented {
                // Never aligned with line ends, like a real socket.
                for piece in body.as_bytes().chunks(5) {
                    stream.write_all(piece).unwrap();
                }
            } else {
                stream.write_all(body.as_bytes()).unwrap();
            }
        }
    });
    FakeServer {
        base_url: format!("http://{addr}"),
        requests,
    }
}

/// Reads one HTTP request and returns just its body. Shared with the
/// abort scripts, which need to see the request a stalled stream is answering.
pub(super) fn read_request_body(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "connection closed before headers finished");
        buf.extend_from_slice(&chunk[..n]);
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
    let length: usize = headers
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while buf.len() < header_end + 4 + length {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "connection closed before body finished");
        buf.extend_from_slice(&chunk[..n]);
    }
    String::from_utf8_lossy(&buf[header_end + 4..header_end + 4 + length]).to_string()
}

pub(super) fn text_response(text: &str) -> String {
    json!({
        "choices": [{"message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
    })
    .to_string()
}

/// An answer the provider ran out of tokens on: same shape, `length`
/// finish (GAP 11 scripts).
pub(super) fn length_response(text: &str) -> String {
    json!({
        "choices": [{"message": {"role": "assistant", "content": text}, "finish_reason": "length"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
    })
    .to_string()
}

/// A turn the model spent entirely on thinking before `max_tokens` ran
/// out: `length` finish, no content, reasoning only.
pub(super) fn thought_only_length_response(reasoning: &str) -> String {
    json!({
        "choices": [{
            "message": {"role": "assistant", "content": null, "reasoning_content": reasoning},
            "finish_reason": "length",
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 512, "total_tokens": 522,
                  "reasoning_tokens": 512},
    })
    .to_string()
}

/// `(id, name, arguments-string)` triples become one assistant tool-call
/// response with `content: null`.
pub(super) fn tool_call_response(calls: &[(&str, &str, &str)]) -> String {
    tool_call_response_with(calls, "tool_calls")
}

/// The same tool-call batch cut by `max_tokens` — the shape a truncated
/// turn arrives as when it was mid-tool-call (GAP 2 + GAP 11 scripts).
pub(super) fn length_tool_call_response(calls: &[(&str, &str, &str)]) -> String {
    tool_call_response_with(calls, "length")
}

fn tool_call_response_with(calls: &[(&str, &str, &str)], finish_reason: &str) -> String {
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": calls.iter().map(|(id, name, args)| json!({
                    "id": id, "type": "function",
                    "function": {"name": name, "arguments": args},
                })).collect::<Vec<_>>(),
            },
            "finish_reason": finish_reason,
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": 8, "total_tokens": 28},
    })
    .to_string()
}

// The SSE counterparts, mirroring Strata's stream shape (keep-alive
// comments, role echo, fragments, empty-delta finish, usage chunk,
// [DONE]) with the SAME usage numbers as the JSON builders, so streamed
// and non-streamed scripts record identical sessions.

fn sse_stream(events: Vec<Value>) -> String {
    let mut out = String::from(": keep-alive\n\n");
    for event in events {
        out.push_str(&format!("data: {event}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
    out
}

/// [`text_response`](text_response)'s stream: the text split into two
/// content deltas.
pub(super) fn sse_text_response(text: &str) -> String {
    let mid = text.char_indices().count() / 2;
    let split = text
        .char_indices()
        .nth(mid)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    let (a, b) = text.split_at(split);
    sse_stream(vec![
        json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"content":a},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"content":b},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}),
    ])
}

/// [`text_response`](text_response)'s stream cut off mid-answer: the text
/// deltas arrive, then the connection ends — no finish chunk, no usage
/// chunk, no `[DONE]` (the GAP 2 shape: a clean EOF from a dead upstream).
pub(super) fn sse_text_response_cut(text: &str) -> String {
    let mid = text.char_indices().count() / 2;
    let split = text
        .char_indices()
        .nth(mid)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    let (a, b) = text.split_at(split);
    let mut out = String::from(": keep-alive\n\n");
    for event in [
        json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"content":a},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"content":b},"finish_reason":null}]}),
    ] {
        out.push_str(&format!("data: {event}\n\n"));
    }
    out
}

/// [`text_response`](text_response)'s stream with thinking in front
/// (Strata's order: reasoning fragments, then content): the reasoning
/// split into two fragments, the text into two.
pub(super) fn sse_reasoning_text_response(reasoning: &str, text: &str) -> String {
    let split_at = |s: &str| {
        let mid = s.char_indices().count() / 2;
        s.char_indices().nth(mid).map(|(i, _)| i).unwrap_or(s.len())
    };
    let (ra, rb) = reasoning.split_at(split_at(reasoning));
    let (a, b) = text.split_at(split_at(text));
    sse_stream(vec![
        json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"reasoning_content":ra},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"reasoning_content":rb},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"content":a},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"content":b},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}),
    ])
}

/// [`thought_only_length_response`](thought_only_length_response)'s
/// stream: reasoning fragments, then the `length` finish.
pub(super) fn sse_thought_only_length_response(reasoning: &str) -> String {
    sse_stream(vec![
        json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"reasoning_content":reasoning},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"length"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":512,"total_tokens":522}}),
    ])
}

/// [`tool_call_response`](tool_call_response)'s stream: id and name on the
/// first fragment, the arguments string split mid-JSON. The role echo
/// carries `content: ""` exactly like real Strata — the shape that once
/// produced a `content: ""` tool-call entry (M5 review bug #2), which
/// the streamed-vs-plain parity assertion now guards against.
pub(super) fn sse_tool_call_response(calls: &[(&str, &str, &str)]) -> String {
    let mut events = vec![json!(
        {"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}
    )];
    for (i, (id, name, args)) in calls.iter().enumerate() {
        let (a, b) = args.split_at(args.len() / 2);
        events.push(json!({
            "choices":[{"index":0,"delta":{"tool_calls":[
                {"index":i,"id":id,"type":"function","function":{"name":name,"arguments":a}}
            ]},"finish_reason":null}]
        }));
        events.push(json!({
            "choices":[{"index":0,"delta":{"tool_calls":[
                {"index":i,"function":{"arguments":b}}
            ]},"finish_reason":null}]
        }));
    }
    events.push(json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}));
    events.push(
        json!({"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":8,"total_tokens":28}}),
    );
    sse_stream(events)
}
