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
//! `choices: []` chunk carrying `usage`, and `timings` objects that are
//! parsed into [`ChatResponse::timings`].
//!
//! A body that ends before its sentinel is salvage, not loss (GAP 2): the
//! user has already watched most of an answer stream in, so
//! [`drain_sse`] reports what assembled plus `complete: false` and lets
//! the loop continue the answer instead of throwing the turn away.
//!
//! A stop request is a different animal (GAP 3's in-flight half): nobody
//! waits for it, so the read watches the run's abort flag while it reads
//! and comes back with the partial and `aborted: true`.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context;

use super::types::{ChatMessage, ChatResponse, Choice, Timings, ToolCall, Usage};

mod chunk;

use chunk::StreamChunk;

/// One SSE chunk, permissive like every wire type (§8): `choices` is empty
/// on the usage-only final chunk, `delta` can be `{}` on the chunk that
/// carries `finish_reason`.
/// What one stream fragment carries, classified at the read: visible
/// text or thinking. The loop maps these straight onto `AgentEvent`
/// variants; everything downstream stays kind-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    Text(String),
    Thinking(String),
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
    timings: Option<Timings>,
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
        if chunk.timings.is_some() {
            self.timings = chunk.timings;
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
            timings: self.timings,
        }
    }
}

/// What a finished stream produced: the assembled response, and whether
/// the wire said so.
///
/// `complete` is true only when the protocol's sentinel (`[DONE]`,
/// `message_stop`) arrived. A cut body that assembled text, thinking or
/// tool calls still comes back `Ok` — discarding an answer the user has
/// already watched stream in is the wrong trade (GAP 2) — and the loop
/// uses the flag to ask for the rest. A cut that assembled *nothing* is
/// an error: there is nothing to salvage and nothing to continue from.
///
/// `aborted` says who stopped it: the user, not the wire. A user
/// abort always comes back `Ok` — empty partial included — and the loop
/// must read it as "the run is over", never as "this answer is short, ask
/// for the rest".
#[derive(Debug, PartialEq)]
pub struct StreamOutcome {
    pub response: ChatResponse,
    pub complete: bool,
    /// The run's stop request ended the read.
    pub aborted: bool,
}

/// How often a stream read looks at the abort flag: responsive enough
/// that a stop request feels immediate, cheap enough to be noise. The
/// request timeout is minutes, so this is the only thing between the user
/// and a generation that has another ten minutes to run.
pub(super) const ABORT_POLL: Duration = Duration::from_millis(100);

/// Await a future, giving up when the run's stop request says so.
///
/// Dropping the future mid-way drops the `&mut Response` it borrowed, and
/// with it the connection: the read is really over, not merely abandoned.
pub(super) async fn until_abort<T>(abort: &AtomicBool, fut: impl Future<Output = T>) -> Option<T> {
    tokio::select! {
        value = fut => Some(value),
        _ = wait_for_abort(abort) => None,
    }
}

/// Resolve once the run's stop request is set — polled, not signalled:
/// the flag is a plain `AtomicBool` every layer already holds, and making
/// it wake anyone would mean a channel on the hot path of every run.
async fn wait_for_abort(abort: &AtomicBool) {
    while !abort.load(Ordering::Relaxed) {
        tokio::time::sleep(ABORT_POLL).await;
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
/// A stream that ends without the `[DONE]` sentinel is reported, never
/// silently accepted (§17) and never thrown away (GAP 2): Strata serves
/// SSE with `Connection: close` and no framing checksum, so a
/// mid-generation server death arrives as a clean EOF. What assembled so
/// far comes back with `complete: false`; only an empty partial — nothing
/// text, thinking or tool-call shaped — is an error worth failing the run
/// for. (An `Err` also stays the verdict for malformed chunks: a provider
/// whose JSON does not parse is not a salvageable truncation.)
///
/// A stop request cuts the read short by the same route and is
/// never an error: what assembled so far comes back with `aborted: true`,
/// and an empty partial is fine — the user asked for nothing more.
pub(super) async fn drain_sse(
    mut response: reqwest::Response,
    abort: &AtomicBool,
    mut on_delta: impl FnMut(Delta) -> Pin<Box<dyn Future<Output = ()> + Send>>,
) -> anyhow::Result<StreamOutcome> {
    let mut lines = LineBuffer::new();
    let mut assembler = Assembler::default();
    let mut done = false;
    let mut aborted = false;
    while !done {
        let Some(bytes) = until_abort(abort, response.chunk()).await else {
            aborted = true;
            break;
        };
        let Some(bytes) = bytes.context("reading SSE stream")? else {
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
    let response = assembler.finish();
    if !(done || aborted) {
        anyhow::ensure!(
            salvageable(&response),
            "SSE stream ended without [DONE] and assembled nothing — the connection was \
             cut before the model produced anything (nothing to salvage; retry the run)"
        );
    }
    Ok(StreamOutcome {
        response,
        complete: done,
        aborted,
    })
}

/// Whether an assembled response is worth keeping when its stream was
/// cut: any visible text, thinking, or tool call counts.
pub(super) fn salvageable(response: &ChatResponse) -> bool {
    response.choices.iter().any(|choice| {
        choice
            .message
            .content
            .as_deref()
            .is_some_and(|text| !text.is_empty())
            || choice
                .message
                .reasoning_content
                .as_deref()
                .is_some_and(|text| !text.is_empty())
            || choice
                .message
                .reasoning
                .as_deref()
                .is_some_and(|text| !text.is_empty())
            || !choice.message.tool_calls.is_empty()
    })
}

#[cfg(test)]
mod tests;
