//! SSE decoding for Anthropic Messages (`/v1/messages`) streaming responses
//! — the same salvage contract as [`crate::llm::stream`]: a body that ends
//! before `message_stop` comes back with what assembled and
//! `complete: false`, and only an empty partial is an error. A stop request
//! is the other way a read ends: polled while waiting for the next
//! fragment, answered with the partial and `aborted: true`.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;

use anyhow::Context;
use serde_json::Value;

use super::map_stop_reason;
use crate::llm::stream::{Delta, LineBuffer, StreamOutcome, salvageable, until_abort};
use crate::llm::types::{ChatMessage, ChatResponse, Choice, ToolCall, Usage};

#[derive(Default)]
struct PartialToolUse {
    id: String,
    name: String,
    initial_input: Option<Value>,
    json_buf: String,
}

#[derive(Default)]
pub(super) struct AnthropicAssembler {
    content: String,
    reasoning: String,
    stop_reason: Option<String>,
    input_tokens: u32,
    cache_read_tokens: u32,
    cache_creation_tokens: u32,
    output_tokens: u32,
    saw_usage: bool,
    tool_uses: BTreeMap<u32, PartialToolUse>,
}

impl AnthropicAssembler {
    pub(super) fn feed(
        &mut self,
        event_hint: Option<&str>,
        val: &Value,
    ) -> anyhow::Result<Vec<Delta>> {
        let kind = val
            .get("type")
            .and_then(Value::as_str)
            .or(event_hint)
            .unwrap_or("");
        let mut deltas = Vec::new();

        match kind {
            "message_start" => {
                if let Some(usage) = val.pointer("/message/usage") {
                    self.saw_usage = true;
                    if let Some(n) = usage.get("input_tokens").and_then(Value::as_u64) {
                        self.input_tokens = n as u32;
                    }
                    if let Some(n) = usage.get("cache_read_input_tokens").and_then(Value::as_u64) {
                        self.cache_read_tokens = n as u32;
                    }
                    if let Some(n) = usage
                        .get("cache_creation_input_tokens")
                        .and_then(Value::as_u64)
                    {
                        self.cache_creation_tokens = n as u32;
                    }
                    if let Some(n) = usage.get("output_tokens").and_then(Value::as_u64) {
                        self.output_tokens = n as u32;
                    }
                }
            }
            "content_block_start" => {
                let index = val.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                if let Some(block) = val.get("content_block") {
                    match block.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text" => {
                            if let Some(t) = block.get("text").and_then(Value::as_str)
                                && !t.is_empty()
                            {
                                self.content.push_str(t);
                                deltas.push(Delta::Text(t.to_string()));
                            }
                        }
                        "thinking" => {
                            if let Some(t) = block.get("thinking").and_then(Value::as_str)
                                && !t.is_empty()
                            {
                                self.reasoning.push_str(t);
                                deltas.push(Delta::Thinking(t.to_string()));
                            }
                        }
                        "tool_use" => {
                            let id = block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let initial_input = block.get("input").cloned();
                            self.tool_uses.insert(
                                index,
                                PartialToolUse {
                                    id,
                                    name,
                                    initial_input,
                                    json_buf: String::new(),
                                },
                            );
                        }
                        _ => {}
                    }
                }
            }
            "content_block_delta" => {
                let index = val.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                if let Some(delta) = val.get("delta") {
                    match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text_delta" => {
                            if let Some(t) = delta.get("text").and_then(Value::as_str)
                                && !t.is_empty()
                            {
                                self.content.push_str(t);
                                deltas.push(Delta::Text(t.to_string()));
                            }
                        }
                        "thinking_delta" => {
                            if let Some(t) = delta.get("thinking").and_then(Value::as_str)
                                && !t.is_empty()
                            {
                                self.reasoning.push_str(t);
                                deltas.push(Delta::Thinking(t.to_string()));
                            }
                        }
                        "input_json_delta" => {
                            if let Some(part) = delta.get("partial_json").and_then(Value::as_str)
                                && let Some(tool) = self.tool_uses.get_mut(&index)
                            {
                                tool.json_buf.push_str(part);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(reason) = val.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(reason.to_string());
                }
                if let Some(usage) = val.get("usage") {
                    self.saw_usage = true;
                    if let Some(n) = usage.get("output_tokens").and_then(Value::as_u64) {
                        self.output_tokens = n as u32;
                    }
                    if let Some(n) = usage.get("input_tokens").and_then(Value::as_u64)
                        && n > 0
                    {
                        self.input_tokens = n as u32;
                    }
                }
            }
            "error" => {
                let err_type = val
                    .pointer("/error/type")
                    .and_then(Value::as_str)
                    .unwrap_or("api_error");
                let err_msg = val
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown stream error");
                anyhow::bail!("Anthropic stream error ({err_type}): {err_msg}");
            }
            _ => {}
        }
        Ok(deltas)
    }

    pub(super) fn finish(self) -> ChatResponse {
        let tool_calls = self
            .tool_uses
            .into_values()
            .map(|t| {
                let args = if !t.json_buf.is_empty() {
                    t.json_buf
                } else if let Some(v) = t.initial_input
                    && !v.as_object().is_some_and(|m| m.is_empty())
                {
                    serde_json::to_string(&v).unwrap_or_else(|_| "{}".to_string())
                } else {
                    "{}".to_string()
                };
                ToolCall::function(t.id, t.name, args)
            })
            .collect();

        let usage = if self.saw_usage {
            let prompt_tokens = self
                .input_tokens
                .saturating_add(self.cache_read_tokens)
                .saturating_add(self.cache_creation_tokens);
            let completion_tokens = self.output_tokens;
            Some(Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens.saturating_add(completion_tokens),
            })
        } else {
            None
        };

        ChatResponse {
            choices: vec![Choice {
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: (!self.content.is_empty()).then_some(self.content),
                    tool_calls,
                    reasoning_content: (!self.reasoning.is_empty()).then_some(self.reasoning),
                    ..ChatMessage::default()
                },
                finish_reason: map_stop_reason(self.stop_reason),
            }],
            usage,
        }
    }
}

pub(crate) async fn drain_sse(
    mut response: reqwest::Response,
    abort: &AtomicBool,
    mut on_delta: impl FnMut(Delta) -> Pin<Box<dyn Future<Output = ()> + Send>>,
) -> anyhow::Result<StreamOutcome> {
    let mut lines = LineBuffer::new();
    let mut assembler = AnthropicAssembler::default();
    let mut current_event: Option<String> = None;
    let mut done = false;
    let mut aborted = false;

    while !done {
        let Some(bytes) = until_abort(abort, response.chunk()).await else {
            aborted = true;
            break;
        };
        let Some(bytes) = bytes.context("reading Anthropic SSE stream")? else {
            break;
        };
        for line in lines.feed(&bytes) {
            if line.is_empty() {
                current_event = None;
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            if let Some(ev) = line.strip_prefix("event:") {
                current_event = Some(ev.trim().to_string());
                continue;
            }
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.strip_prefix(' ').unwrap_or(data);
            let val: Value = serde_json::from_str(data)
                .with_context(|| format!("parsing Anthropic SSE chunk {data}"))?;
            let is_stop = val
                .get("type")
                .and_then(Value::as_str)
                .or(current_event.as_deref())
                == Some("message_stop");
            if is_stop {
                done = true;
                break;
            }
            for delta in assembler.feed(current_event.as_deref(), &val)? {
                on_delta(delta).await;
            }
        }
    }

    let assembled = assembler.finish();
    if !(done || aborted) {
        anyhow::ensure!(
            salvageable(&assembled),
            "Anthropic SSE stream ended without message_stop and assembled nothing — the \
             connection was cut before the model produced anything (nothing to salvage; \
             retry the run)"
        );
    }
    Ok(StreamOutcome {
        response: assembled,
        complete: done,
        aborted,
    })
}
