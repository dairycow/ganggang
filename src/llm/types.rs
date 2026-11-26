//! Wire types for an OpenAI-compatible /v1/chat/completions request & response.
//!
//! RULE FOR TALKING TO LLM PROVIDERS: be PERMISSIVE on the way in.
//! Providers add, drop and rename fields constantly. serde ignores unknown
//! JSON keys by default, which is exactly what we want — never add
//! #[serde(deny_unknown_fields)].
//!
//! THE OTHER HALF OF THAT RULE: a plain `T` field is a DEMAND that the key
//! exists (`missing field "x"` at runtime). `Option<T>` or `#[serde(default)]`
//! makes it optional. So only declare fields we actually need — every extra
//! field is another way a real provider can break us. (M1 review: `id`,
//! `model`, `index` and `type` were all required and none were ever read.)
//!
//! LEARNING NOTE — where serde errors happen:
//!   compile time: Rust type mistakes (wrong field name in your own code,
//!                 keyword used as an identifier, missing struct)
//!   runtime:      Rust type vs JSON shape mismatches. Rust CANNOT check that
//!                 `Vec<String>` matches an array of objects. Hence: tests.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize, PartialEq)]
pub struct ChatResponse {
    // `Vec`, not a single Choice: n>1 requests and some providers' shapes vary.
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
    #[serde(default)]
    pub timings: Option<Timings>,
}

/// Permissive inference timing telemetry (e.g. Strata / llama.cpp `timings`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Timings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_n: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_per_second: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicted_n: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicted_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicted_per_second: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_n: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_n: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_n_accepted: Option<u32>,
}

impl Timings {
    /// Decode throughput in tokens/sec, preferring `predicted_per_second`
    /// and falling back to `predicted_n / (predicted_ms / 1000.0)`.
    pub fn decode_tok_per_sec(&self) -> Option<f64> {
        if let Some(tps) = self
            .predicted_per_second
            .filter(|t| t.is_finite() && *t > 0.0)
        {
            return Some(tps);
        }
        match (self.predicted_n, self.predicted_ms) {
            (Some(n), Some(ms)) if n > 0 && ms.is_finite() && ms > 0.0 => {
                Some(n as f64 / (ms / 1000.0))
            }
            _ => None,
        }
    }

    /// Prompt prefill throughput in tokens/sec, preferring `prompt_per_second`
    /// and falling back to `prompt_n / (prompt_ms / 1000.0)`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn prompt_tok_per_sec(&self) -> Option<f64> {
        if let Some(tps) = self.prompt_per_second.filter(|t| t.is_finite() && *t > 0.0) {
            return Some(tps);
        }
        match (self.prompt_n, self.prompt_ms) {
            (Some(n), Some(ms)) if n > 0 && ms.is_finite() && ms > 0.0 => {
                Some(n as f64 / (ms / 1000.0))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Choice {
    pub message: ChatMessage,
    // "stop" | "tool_calls" | "length" | ...
    // String, not enum: providers disagree on the exact set, and an unknown
    // variant would fail the parse. Upgrade later to an enum with #[serde(other)].
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// ONE message type for every direction, because a conversation must be able
/// to REPLAY itself. An assistant message that requested a tool, and the
/// `role: "tool"` message carrying that tool's result, both have to go back out
/// on the next request. Two separate request/response message types cannot do
/// that — that's why M1's `RequestMessage` is gone.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    // "user" | "assistant" | "system" | "tool"
    pub role: String,
    // None for a pure tool-call message (server sends "content": null), and
    // None for a system prompt in some providers.
    //
    // skip_serializing_if: when None, the KEY IS OMITTED from the JSON we send
    // rather than written as null. Several servers reject "content": null.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    // Only present on assistant messages that ask for a tool.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    // Only present on role:"tool" messages: links this result back to the
    // ToolCall it is answering. Without it, providers reject the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    // Thinking text (Strata/Qwen emit `reasoning_content` as a sibling of
    // `content`, streamed as fragments before the visible text). Captured
    // on the way IN for display (M7.3) and populated on replay according to
    // `ReplayReasoning` (`Never` | `Turn` | `All`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    // Some OpenAI-compatible providers emit `"reasoning"` instead of
    // `"reasoning_content"`. Captured on the way in; never serialized out.
    #[serde(default, skip_serializing)]
    pub reasoning: Option<String>,
    // Internal flag for Anthropic `tool_result` conversion (`is_error: true`).
    // Never serialized on the OpenAI wire.
    #[serde(skip)]
    pub is_error: bool,
}

fn default_tool_type() -> String {
    "function".to_string()
}

/// One entry in `message.tool_calls[]`.
///
/// The nesting is easy to misread: `"type": "function"` is a STRING, while
/// `"function": { ... }` is an OBJECT. Two keys at the same level, one string,
/// one object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_tool_type")]
    pub kind: String,
    pub function: FunctionCall,
}

impl ToolCall {
    pub fn function(id: String, name: String, arguments: String) -> Self {
        Self {
            id,
            kind: default_tool_type(),
            function: FunctionCall { name, arguments },
        }
    }
}

/// The `function` object inside a ToolCall.
/// `arguments` is a STRING CONTAINING JSON: "{\"expression\":\"12*9\"}" —
/// the model streams tool arguments as raw text, so providers hand it over
/// unparsed. M2 parses it inside each tool, and treats bad/empty arguments as
/// a tool ERROR fed back to the model, never an `?` that aborts the whole loop
/// (models emit `""` for no-arg tools constantly).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct CompletionTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    #[serde(default)]
    pub reasoning_tokens: Option<u32>,
    #[serde(default)]
    pub completion_tokens_details: Option<CompletionTokensDetails>,
    /// Cache-read tokens (prefix-cache hits), once the provider reports
    /// them; folded into `prompt_tokens`, never added on top.
    #[serde(default)]
    pub cache_read_tokens: u32,
}

impl Usage {
    /// Reasoning tokens reported either top-level or nested under
    /// `completion_tokens_details.reasoning_tokens`.
    pub fn effective_reasoning_tokens(&self) -> Option<u32> {
        self.reasoning_tokens.or_else(|| {
            self.completion_tokens_details
                .as_ref()
                .and_then(|d| d.reasoning_tokens)
        })
    }
}

/// The outbound request. Built by LlmClient::send, not by callers.
#[derive(Debug, Serialize)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    // Slice, not Vec: we borrow the caller's history instead of cloning it.
    // `&Vec<ChatMessage>` coerces to `&[ChatMessage]` automatically.
    pub messages: &'a [ChatMessage],
    pub max_tokens: u32,
    // OpenAI function declarations, borrowed from the tool registry. The
    // schema literal lives with each tool, not here.
    #[serde(skip_serializing_if = "<[serde_json::Value]>::is_empty")]
    pub tools: &'a [Value],
    // Streaming (M5): `Some(true)` switches the endpoint to SSE chunks.
    // Absent on the non-streaming path — same request shape either way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    // Ask the server to append a final chunk carrying `usage` with empty
    // `choices` (Strata honors it; without it a streamed run has no usage,
    // and the footer's context % would be blind).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_budget_tokens: Option<u32>,
}

/// The `stream_options` object of a streaming request.
#[derive(Debug, Clone, Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_a_request() {
        let messages = [ChatMessage {
            role: "user".to_string(),
            content: Some("hi".to_string()),
            ..Default::default()
        }];
        let req = ChatRequest {
            model: "m",
            messages: &messages,
            max_tokens: 512,
            tools: &[],
            stream: None,
            stream_options: None,
            reasoning_effort: None,
            reasoning_budget_tokens: None,
        };

        // skip_serializing_if drops the empty tool_calls, None tool_call_id and
        // empty tools, so the wire form stays minimal. Field order follows
        // declaration order.
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"model":"m","messages":[{"role":"user","content":"hi"}],"max_tokens":512}"#
        );
    }

    /// The streaming request carries `stream` and `stream_options` and
    /// nothing else changes shape.
    #[test]
    fn serializes_a_streaming_request() {
        let messages = [ChatMessage {
            role: "user".to_string(),
            content: Some("hi".to_string()),
            ..Default::default()
        }];
        let req = ChatRequest {
            model: "m",
            messages: &messages,
            max_tokens: 512,
            tools: &[],
            stream: Some(true),
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            reasoning_effort: None,
            reasoning_budget_tokens: None,
        };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"model":"m","messages":[{"role":"user","content":"hi"}],"max_tokens":512,"stream":true,"stream_options":{"include_usage":true}}"#
        );
    }

    /// The request `tools` array uses the OpenAI function-declaration shape;
    /// the schema literal is built by each tool, so this only pins the wrapper.
    #[test]
    fn serializes_a_request_with_tools() {
        let messages = [ChatMessage {
            role: "user".to_string(),
            content: Some("hi".to_string()),
            ..Default::default()
        }];
        let declaration = serde_json::json!({
            "type": "function",
            "function": {
                "name": "echo",
                "description": "echo the text",
                "parameters": {
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"]
                }
            }
        });
        let req = ChatRequest {
            model: "m",
            messages: &messages,
            max_tokens: 512,
            tools: std::slice::from_ref(&declaration),
            stream: None,
            stream_options: None,
            reasoning_effort: None,
            reasoning_budget_tokens: None,
        };

        let json = serde_json::to_string(&req).unwrap();
        // serde_json's map ordering is not the wire ordering, so compare
        // parsed values, not strings — the shape is what matters here.
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed["tools"],
            serde_json::json!([declaration]),
            "raw: {json}"
        );
        assert_eq!(parsed["model"], "m");
        assert_eq!(parsed["max_tokens"], 512);
    }

    /// The simple case: a plain text answer.
    #[test]
    fn parses_a_plain_text_response() {
        let raw = include_str!("../../fixtures/text_response.json");
        let resp: ChatResponse = serde_json::from_str(raw).unwrap();

        assert_eq!(resp.choices.len(), 1);

        // `as_deref` turns Option<String> into Option<&str> so we can compare
        // against a string literal without allocating a String.
        assert_eq!(
            resp.choices[0].message.content.as_deref(),
            Some("ganggang online")
        );
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(resp.usage.as_ref().unwrap().completion_tokens, 32);
        assert!(resp.choices[0].message.tool_calls.is_empty());
    }

    /// The interesting case: the model asked for a tool.
    #[test]
    fn parses_a_tool_call_response() {
        let raw = include_str!("../../fixtures/tool_call_response.json");
        let resp: ChatResponse = serde_json::from_str(raw).unwrap();

        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("tool_calls"));

        let msg = &resp.choices[0].message;
        assert_eq!(msg.tool_calls.len(), 1);

        let tc = &msg.tool_calls[0];
        assert_eq!(tc.function.name, "calculator");
        assert!(tc.function.arguments.contains("12*9"));
        assert!(tc.id.starts_with("call_"));

        // A tool-call message carries NO content. Comparing the Option directly
        // beats unwrapping — a panic here would hide which side went wrong.
        assert_eq!(msg.content, None);

        // `arguments` is JSON trapped inside a String.
        let args: serde_json::Value = serde_json::from_str(&tc.function.arguments).unwrap();
        assert_eq!(args["expression"].as_str(), Some("12*9"));
    }

    /// The test M1 was missing and M2 depends on: an assistant message that
    /// requested a tool must survive a round trip so it can be REPLAYED in the
    /// next request. If skip_serializing_if or `default` were wrong, this breaks.
    #[test]
    fn tool_call_message_round_trips_for_replay() {
        let raw = include_str!("../../fixtures/tool_call_response.json");
        let resp: ChatResponse = serde_json::from_str(raw).unwrap();
        let msg = resp.choices.into_iter().next().unwrap().message;

        let wire = serde_json::to_string(&msg).unwrap();
        let back: ChatMessage = serde_json::from_str(&wire).unwrap();

        assert_eq!(back.role, "assistant");
        assert_eq!(back.content, None);
        assert_eq!(back.tool_calls.len(), 1);
        assert_eq!(back.tool_calls[0].function.name, "calculator");
        assert_eq!(back.tool_calls[0].id, msg.tool_calls[0].id);
    }

    /// A role:"tool" result message must serialize without inventing a null
    /// content key or tool_calls array, and must carry tool_call_id.
    #[test]
    fn serializes_a_tool_result_message() {
        let msg = ChatMessage {
            role: "tool".to_string(),
            content: Some("108".to_string()),
            tool_call_id: Some("call_1".to_string()),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"role":"tool","content":"108","tool_call_id":"call_1"}"#
        );
    }

    #[test]
    fn tool_call_serializes_type_function_and_deserializes_without_type() {
        let tc = ToolCall::function("call_1".into(), "read".into(), "{}".into());
        let json = serde_json::to_value(&tc).unwrap();
        assert_eq!(json["type"], "function");

        let back: ToolCall =
            serde_json::from_str(r#"{"id":"call_1","function":{"name":"read","arguments":"{}"}}"#)
                .unwrap();
        assert_eq!(back.kind, "function");
    }

    #[test]
    fn request_omits_or_includes_reasoning_effort() {
        let messages = [ChatMessage {
            role: "user".to_string(),
            content: Some("hi".to_string()),
            ..Default::default()
        }];
        let req = ChatRequest {
            model: "m",
            messages: &messages,
            max_tokens: 512,
            tools: &[],
            stream: None,
            stream_options: None,
            reasoning_effort: Some("low"),
            reasoning_budget_tokens: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["reasoning_effort"], "low");
        assert!(v.get("reasoning_budget_tokens").is_none());
    }

    #[test]
    fn request_omits_or_includes_reasoning_budget_tokens() {
        let messages = [ChatMessage {
            role: "user".to_string(),
            content: Some("hi".to_string()),
            ..Default::default()
        }];
        let req = ChatRequest {
            model: "m",
            messages: &messages,
            max_tokens: 512,
            tools: &[],
            stream: None,
            stream_options: None,
            reasoning_effort: None,
            reasoning_budget_tokens: Some(4096),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["reasoning_budget_tokens"], 4096);
    }

    #[test]
    fn message_deserializes_reasoning_fallback_without_colliding() {
        let msg: ChatMessage =
            serde_json::from_str(r#"{"role":"assistant","content":"hi","reasoning":"thought"}"#)
                .unwrap();
        assert_eq!(msg.reasoning.as_deref(), Some("thought"));
        assert_eq!(msg.reasoning_content, None);
    }

    #[test]
    fn timings_deserializes_and_computes_fallbacks() {
        let raw = r#"{"cache_n":0,"prompt_n":42,"prompt_ms":1653.5,"prompt_per_second":25.4,"predicted_n":22,"predicted_ms":520.2,"predicted_per_second":42.3,"draft_n":18,"draft_n_accepted":12}"#;
        let t: Timings = serde_json::from_str(raw).unwrap();
        assert_eq!(t.decode_tok_per_sec(), Some(42.3));
        assert_eq!(t.prompt_tok_per_sec(), Some(25.4));
        assert_eq!(t.draft_n, Some(18));
        assert_eq!(t.draft_n_accepted, Some(12));

        let fixture: ChatResponse =
            serde_json::from_str(include_str!("../../fixtures/tool_call_response.json")).unwrap();
        let ft = fixture.timings.expect("fixture timings");
        assert_eq!(ft.draft_n, Some(88));
        assert_eq!(ft.draft_n_accepted, Some(51));

        let fallback = Timings {
            predicted_n: Some(100),
            predicted_ms: Some(2000.0),
            prompt_n: Some(50),
            prompt_ms: Some(500.0),
            ..Default::default()
        };
        assert_eq!(fallback.decode_tok_per_sec(), Some(50.0));
        assert_eq!(fallback.prompt_tok_per_sec(), Some(100.0));

        let bad = Timings {
            predicted_per_second: Some(0.0),
            predicted_n: Some(10),
            predicted_ms: Some(f64::NAN),
            prompt_per_second: Some(f64::INFINITY),
            prompt_n: Some(10),
            prompt_ms: Some(-1.0),
            ..Default::default()
        };
        assert_eq!(bad.decode_tok_per_sec(), None);
        assert_eq!(bad.prompt_tok_per_sec(), None);
    }

    #[test]
    fn usage_effective_reasoning_tokens_handles_top_level_and_nested() {
        let top: Usage = serde_json::from_str(
            r#"{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30,"reasoning_tokens":12}"#,
        )
        .unwrap();
        assert_eq!(top.effective_reasoning_tokens(), Some(12));

        let nested: Usage = serde_json::from_str(
            r#"{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30,"completion_tokens_details":{"reasoning_tokens":15}}"#,
        )
        .unwrap();
        assert_eq!(nested.effective_reasoning_tokens(), Some(15));
    }
}
