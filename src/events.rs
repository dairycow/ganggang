//! Everything the outside world can observe about an agent run.
//!
//! The TUI renders these, `--mode json` serializes them line by line, and
//! print mode prints the last `AssistantText`. One enum keeps the three
//! interfaces from drifting apart.

use serde::Serialize;
use serde_json::Value;

/// `TurnEnded::stop_reason` values that mean "this turn fell short of an
/// answer": the provider ran out of tokens, or the stream ended before
/// its sentinel. The loop's `cut` module emits them; interfaces match on
/// them (print mode accumulates a continued answer rather than letting
/// the next turn replace the last one). Both are ordinary `String`
/// payloads — no new event variant is needed to say it.
pub const STOP_LENGTH: &str = "length";
pub const STOP_STREAM_CUT: &str = "stream_cut";

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    RunStarted,
    TurnStarted,
    AssistantText {
        delta: String,
    },
    /// Thinking fragment (`reasoning_content`, M7.3): emitted per chunk,
    /// like `AssistantText`. Interfaces opt in to rendering it — the TUI
    /// shows it behind Ctrl+T; print mode omits it; JSON mode passes it
    /// through like every event.
    Thinking {
        delta: String,
    },
    ToolCallStarted {
        name: String,
        id: String,
        args: Value,
    },
    ToolCallFinished {
        name: String,
        id: String,
        output: String,
        is_error: bool,
    },
    TurnEnded {
        stop_reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<crate::agent::session::Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timings: Option<crate::llm::types::Timings>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking_pct: Option<u8>,
    },
    RunEnded {
        turns: u32,
    },
    /// Terminal record of an aborted run (emitted after the `Warned`), so
    /// event consumers — JSON mode, the TUI — get a definite end of stream
    /// that is distinguishable from a clean finish.
    RunAborted,
    Warned {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(event: AgentEvent) -> String {
        serde_json::to_string(&event).unwrap()
    }

    #[test]
    fn markers_serialize_to_their_type_alone() {
        assert_eq!(json(AgentEvent::RunStarted), r#"{"type":"run_started"}"#);
        assert_eq!(json(AgentEvent::TurnStarted), r#"{"type":"turn_started"}"#);
    }

    #[test]
    fn text_and_progress_events_carry_their_payload() {
        assert_eq!(
            json(AgentEvent::AssistantText { delta: "hi".into() }),
            r#"{"type":"assistant_text","delta":"hi"}"#
        );
        assert_eq!(
            json(AgentEvent::Thinking { delta: "hm".into() }),
            r#"{"type":"thinking","delta":"hm"}"#
        );
        assert_eq!(
            json(AgentEvent::TurnEnded {
                stop_reason: "stop".into(),
                usage: None,
                timings: None,
                thinking_pct: None,
            }),
            r#"{"type":"turn_ended","stop_reason":"stop"}"#
        );
        assert_eq!(
            json(AgentEvent::RunEnded { turns: 3 }),
            r#"{"type":"run_ended","turns":3}"#
        );
        assert_eq!(
            json(AgentEvent::Warned {
                message: "truncated".into()
            }),
            r#"{"type":"warned","message":"truncated"}"#
        );
        assert_eq!(json(AgentEvent::RunAborted), r#"{"type":"run_aborted"}"#);
    }

    #[test]
    fn tool_call_events_serialize_snake_case() {
        assert_eq!(
            json(AgentEvent::ToolCallStarted {
                name: "read".into(),
                id: "call_1".into(),
                args: serde_json::json!({"path": "Cargo.toml"}),
            }),
            r#"{"type":"tool_call_started","name":"read","id":"call_1","args":{"path":"Cargo.toml"}}"#
        );
        assert_eq!(
            json(AgentEvent::ToolCallFinished {
                name: "read".into(),
                id: "call_1".into(),
                output: "ok".into(),
                is_error: false,
            }),
            r#"{"type":"tool_call_finished","name":"read","id":"call_1","output":"ok","is_error":false}"#
        );
    }
}
