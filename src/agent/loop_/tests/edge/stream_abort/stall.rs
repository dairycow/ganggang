//! The stalled-stream server the abort scripts run against: it answers
//! with SSE fragments and then says nothing, so only the run's stop request
//! can end the read. It records requests the way the shared fake server
//! does and keeps accepting, so a continuation the loop must not send shows
//! up as a second recorded request instead of a hanging test.

use std::io::Write as _;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::agent::loop_::tests::fake::read_request_body;

/// One SSE answer per connection, then the connection sits open and
/// silent: a stalled generation, not a dead upstream. Only the run's stop
/// request can end that read.
pub(super) struct StallingSse {
    pub(super) base_url: String,
    pub(super) requests: Arc<Mutex<Vec<Value>>>,
}

impl StallingSse {
    pub(super) fn spawn(bodies: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        thread::spawn(move || {
            let mut nth = 0usize;
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request_body(&mut stream);
                seen.lock()
                    .unwrap()
                    .push(serde_json::from_str(&request).unwrap());
                let body = bodies.get(nth).cloned().unwrap_or_default();
                nth += 1;
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
                // Fragmented, never aligned with line ends, like a socket.
                for piece in body.as_bytes().chunks(5) {
                    stream.write_all(piece).unwrap();
                }
                stream.flush().unwrap();
                // Hold it open, well past any deadline the test could set.
                thread::sleep(Duration::from_secs(30));
                drop(stream);
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            requests,
        }
    }

    /// The SSE shape of a stalled generation: role echo, the text split in
    /// two, then silence — no finish chunk, no usage, no `[DONE]`.
    pub(super) fn half_text(text: &str) -> String {
        let mid = text.chars().count() / 2;
        let split = text.char_indices().nth(mid).map(|(i, _)| i).unwrap_or(0);
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

    /// A tool call whose arguments stop mid-JSON, then silence.
    pub(super) fn half_tool_call() -> String {
        let mut out = String::from(": keep-alive\n\n");
        for event in [
            json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"id":"call_cut","type":"function","function":{"name":"echo","arguments":"{\"text\":\"hal"}}
            ]},"finish_reason":null}]}),
        ] {
            out.push_str(&format!("data: {event}\n\n"));
        }
        out
    }
}
