//! SSE decoding for streaming `/chat/completions` responses (spec §15, M5).
//!
//! A streamed response arrives as `data: {chunk}` lines terminated by
//! `data: [DONE]`. Chunks carry partial content in `delta.content` and
//! tool calls as fragments (`index` + pieces of `id`/`name`/`arguments`
//! to concatenate), so the assembler folds them back into the exact
//! [`ChatResponse`] the non-streaming endpoint would have returned — the
//! rest of the stack never learns streaming happened.
//!
//! Thinking (`delta.reasoning_content`, M7.3) is accumulated alongside
//! the visible text and delivered live as [`Delta::Thinking`] — captured
//! for display, never mixed into message content. Strata's stream
//! (verified live) also emits `: keep-alive` comment lines, a final
//! `choices: []` chunk carrying `usage`, and non-standard `timings`
//! objects the permissive wire types ignore.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use anyhow::Context;
use serde::Deserialize;

use super::types::{ChatMessage, ChatResponse, Choice, ToolCall, Usage};

/// One SSE chunk, permissive like every wire type (§8): `choices` is empty
/// on the usage-only final chunk, `delta` can be `{}` on the chunk that
/// carries `finish_reason`.
#[derive(Debug, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Vec<StreamToolCall>,
}

/// What one stream fragment carries, classified at the read: visible
/// text or thinking. The loop maps these straight onto `AgentEvent`
/// variants; everything downstream stays kind-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    Text(String),
    Thinking(String),
}

#[derive(Debug, Deserialize)]
struct StreamToolCall {
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamFunction>,
}

#[derive(Debug, Default, Deserialize)]
struct StreamFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// Folds parsed chunks into a `ChatResponse`. `feed` returns the text
/// deltas encountered so the reader can deliver them live; `finish`
/// produces the assembled response.
#[derive(Default)]
struct Assembler {
    content: String,
    reasoning: String,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    /// BTreeMap: keyed by fragment index, iterated in order at finish.
    calls: BTreeMap<u32, PartialCall>,
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

impl Assembler {
    /// Fold one chunk; returns the non-empty deltas it carried, text and
    /// thinking alike, in arrival order. Only index-0 choices are
    /// assembled — gg never requests n>1.
    fn feed(&mut self, chunk: StreamChunk) -> Vec<Delta> {
        if chunk.usage.is_some() {
            self.usage = chunk.usage;
        }
        let mut deltas = Vec::new();
        for choice in chunk.choices {
            if choice.index != 0 {
                continue;
            }
            if choice.finish_reason.is_some() {
                self.finish_reason = choice.finish_reason;
            }
            let thinking = choice
                .delta
                .reasoning_content
                .filter(|s| !s.is_empty())
                .or_else(|| choice.delta.reasoning.filter(|s| !s.is_empty()));
            if let Some(thinking) = thinking {
                self.reasoning.push_str(&thinking);
                deltas.push(Delta::Thinking(thinking));
            }
            if let Some(text) = choice.delta.content
                && !text.is_empty()
            {
                self.content.push_str(&text);
                deltas.push(Delta::Text(text));
            }
            for fragment in choice.delta.tool_calls {
                let call = self.calls.entry(fragment.index).or_default();
                // `id`/`name` arrive whole on the first fragment; later
                // fragments repeat nothing but the index.
                if let Some(id) = fragment.id
                    && !id.is_empty()
                {
                    call.id = id;
                }
                if let Some(function) = fragment.function {
                    if let Some(name) = function.name
                        && !name.is_empty()
                    {
                        call.name = name;
                    }
                    if let Some(args) = function.arguments {
                        call.arguments.push_str(&args);
                    }
                }
            }
        }
        deltas
    }

    fn finish(self) -> ChatResponse {
        let tool_calls: Vec<ToolCall> = self
            .calls
            .into_values()
            .map(|call| ToolCall::function(call.id, call.name, call.arguments))
            .collect();
        ChatResponse {
            choices: vec![Choice {
                message: ChatMessage {
                    role: "assistant".into(),
                    // `None` unless real text accumulated: the role-echo
                    // chunk carries `content: ""`, which must not turn a
                    // pure tool-call turn into `content: ""` — the shape
                    // has to match the non-streaming wire (`null`).
                    content: (!self.content.is_empty()).then_some(self.content),
                    // Thinking, same minimal-write rule.
                    reasoning_content: (!self.reasoning.is_empty()).then_some(self.reasoning),
                    tool_calls,
                    ..ChatMessage::default()
                },
                finish_reason: self.finish_reason,
            }],
            usage: self.usage,
        }
    }
}

/// Byte-level line splitter. Network chunks do not respect line (or UTF-8
/// character) boundaries, so partial bytes are held back until their line
/// completes; decoding happens per complete line only, which is what keeps
/// a multibyte char split across two reads intact.
pub(super) struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    pub(super) fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Feed raw bytes; returns every complete line now available, without
    /// its terminator (CRLF tolerated per SSE framing).
    pub(super) fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=pos).collect();
            line.pop(); // the '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(String::from_utf8_lossy(&line).into_owned());
        }
        lines
    }
}

/// Read an SSE body to completion, delivering deltas (text and thinking)
/// through `on_delta` as they arrive, and assemble the final
/// `ChatResponse`.
///
/// A stream that ends without the `[DONE]` sentinel is a transport
/// error (§17: every truncation is visible, never silently accepted):
/// Strata serves SSE with `Connection: close` and no framing checksum,
/// so a mid-generation server death arrives as a clean EOF that would
/// otherwise pass for a complete answer.
pub(super) async fn drain_sse(
    mut response: reqwest::Response,
    mut on_delta: impl FnMut(Delta) -> Pin<Box<dyn Future<Output = ()> + Send>>,
) -> anyhow::Result<ChatResponse> {
    let mut lines = LineBuffer::new();
    let mut assembler = Assembler::default();
    let mut done = false;
    while !done {
        let Some(bytes) = response.chunk().await.context("reading SSE stream")? else {
            break;
        };
        for line in lines.feed(&bytes) {
            // SSE comments (`: keep-alive`) and event separators carry no data.
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            let Some(data) = line.strip_prefix("data:") else {
                continue; // other SSE fields (event:, id:) are not sent by this protocol
            };
            let data = data.strip_prefix(' ').unwrap_or(data);
            if data == "[DONE]" {
                done = true;
                break;
            }
            let chunk: StreamChunk =
                serde_json::from_str(data).with_context(|| format!("parsing SSE chunk {data}"))?;
            for delta in assembler.feed(chunk) {
                on_delta(delta).await;
            }
        }
    }
    if !done {
        anyhow::bail!(
            "SSE stream ended without [DONE] — the connection was cut mid-generation \
             (partial output discarded; retry the run)"
        );
    }
    Ok(assembler.finish())
}

#[cfg(test)]
mod tests;
