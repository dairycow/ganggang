//! Anthropic Messages (`/v1/messages`) wire adapter.
//!
//! Translates `ganggang`'s OpenAI-shaped [`ChatMessage`]s and tool
//! declarations into Anthropic's `messages` + top-level `system` +
//! `input_schema` request shape, and folds Anthropic responses back into
//! [`ChatResponse`] so the agent loop and session store stay protocol-agnostic.

pub(super) mod stream;
#[cfg(test)]
mod tests;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::types::{ChatMessage, ChatResponse, Choice, ToolCall, Usage};

pub(super) const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Resolve the `/messages` endpoint URL from a configured `base_url`.
pub(super) fn messages_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        format!("{trimmed}/messages")
    } else {
        format!("{trimmed}/v1/messages")
    }
}

/// Build an Anthropic `/v1/messages` JSON request body.
pub(super) fn build_request(
    model: &str,
    messages: &[ChatMessage],
    max_tokens: u32,
    tools: &[Value],
    stream: bool,
) -> Value {
    let mut system_parts = Vec::new();
    let mut out_messages: Vec<Value> = Vec::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" => {
                if let Some(text) = msg.content.as_deref()
                    && !text.is_empty()
                {
                    system_parts.push(text);
                }
            }
            "user" => {
                let text = msg.content.as_deref().unwrap_or("");
                push_user_block(&mut out_messages, json!({ "type": "text", "text": text }));
            }
            "tool" => {
                let mut block = Map::new();
                block.insert("type".into(), json!("tool_result"));
                block.insert(
                    "tool_use_id".into(),
                    json!(msg.tool_call_id.as_deref().unwrap_or("")),
                );
                block.insert(
                    "content".into(),
                    json!(msg.content.as_deref().unwrap_or("")),
                );
                if msg.is_error {
                    block.insert("is_error".into(), json!(true));
                }
                push_user_block(&mut out_messages, Value::Object(block));
            }
            "assistant" => {
                let mut blocks = Vec::new();
                if let Some(text) = msg.content.as_deref()
                    && !text.is_empty()
                {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
                for tc in &msg.tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.function.name,
                        "input": parse_tool_input(&tc.function.arguments),
                    }));
                }
                if blocks.is_empty() {
                    continue;
                }
                push_assistant_blocks(&mut out_messages, blocks);
            }
            _ => {}
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    body.insert("max_tokens".into(), json!(max_tokens));
    if !system_parts.is_empty() {
        body.insert("system".into(), json!(system_parts.join("\n\n")));
    }
    body.insert("messages".into(), Value::Array(out_messages));

    let anthropic_tools: Vec<Value> = tools
        .iter()
        .filter_map(|t| {
            let func = t.get("function")?;
            let name = func.get("name")?.as_str()?;
            let mut tool = Map::new();
            tool.insert("name".into(), json!(name));
            if let Some(desc) = func.get("description") {
                tool.insert("description".into(), desc.clone());
            }
            tool.insert(
                "input_schema".into(),
                func.get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            );
            Some(Value::Object(tool))
        })
        .collect();
    if !anthropic_tools.is_empty() {
        body.insert("tools".into(), Value::Array(anthropic_tools));
    }
    if stream {
        body.insert("stream".into(), json!(true));
    }
    Value::Object(body)
}

fn push_user_block(out: &mut Vec<Value>, block: Value) {
    if let Some(last) = out.last_mut()
        && last.get("role").and_then(Value::as_str) == Some("user")
        && let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        arr.push(block);
        return;
    }
    out.push(json!({
        "role": "user",
        "content": [block],
    }));
}

fn push_assistant_blocks(out: &mut Vec<Value>, blocks: Vec<Value>) {
    if let Some(last) = out.last_mut()
        && last.get("role").and_then(Value::as_str) == Some("assistant")
        && let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        arr.extend(blocks);
        return;
    }
    out.push(json!({
        "role": "assistant",
        "content": blocks,
    }));
}

fn parse_tool_input(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return json!({});
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(v) if v.is_object() => v,
        _ => json!({}),
    }
}

pub(super) fn map_stop_reason(reason: Option<String>) -> Option<String> {
    match reason.as_deref() {
        Some("end_turn") => Some("stop".to_string()),
        Some("tool_use") => Some("tool_calls".to_string()),
        Some("max_tokens") => Some("length".to_string()),
        _ => reason,
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
}

impl AnthropicUsage {
    fn into_wire(self) -> Usage {
        let prompt_tokens = self
            .input_tokens
            .saturating_add(self.cache_read_input_tokens)
            .saturating_add(self.cache_creation_input_tokens);
        let completion_tokens = self.output_tokens;
        Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
        }
    }
}

/// Parse a non-streaming Anthropic `/v1/messages` JSON response into [`ChatResponse`].
pub(super) fn parse_response(raw: Value) -> anyhow::Result<ChatResponse> {
    let resp: AnthropicResponse = serde_json::from_value(raw)
        .map_err(|e| anyhow::anyhow!("invalid Anthropic response: {e}"))?;

    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();

    for block in resp.content {
        match block.kind.as_str() {
            "text" => {
                if let Some(t) = block.text {
                    content.push_str(&t);
                }
            }
            "thinking" => {
                if let Some(t) = block.thinking {
                    reasoning.push_str(&t);
                }
            }
            "tool_use" => {
                let id = block.id.unwrap_or_default();
                let name = block.name.unwrap_or_default();
                let input = block.input.unwrap_or_else(|| json!({}));
                let args = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                tool_calls.push(ToolCall::function(id, name, args));
            }
            _ => {}
        }
    }

    Ok(ChatResponse {
        choices: vec![Choice {
            message: ChatMessage {
                role: "assistant".to_string(),
                content: (!content.is_empty()).then_some(content),
                tool_calls,
                reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
                ..ChatMessage::default()
            },
            finish_reason: map_stop_reason(resp.stop_reason),
        }],
        usage: resp.usage.map(AnthropicUsage::into_wire),
    })
}
