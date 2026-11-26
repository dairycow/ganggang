//! The continuation budget's two bounds, and the instruction a cut tool
//! call is continued with: the per-chain cap a complete tool turn starts
//! again, and the run's overall cap.

use serde_json::json;

use super::super::fake::{
    FakeServer, length_response, sse_text_response, sse_tool_call_response_cut, text_response,
    tool_call_response,
};
use super::super::{Echo, Registry, drive, drive_streaming, event_values};
use crate::agent::loop_::cut::{
    CONTINUE_INSTRUCTION as CONTINUE, MAX_CONTINUATIONS, MAX_RUN_CONTINUATIONS,
};

const ECHO: (&str, &str, &str) = ("call_1", "echo", r#"{"text":"x"}"#);

fn warned(values: &[serde_json::Value]) -> Vec<&str> {
    values
        .iter()
        .filter(|e| e["type"] == "warned")
        .filter_map(|e| e["message"].as_str())
        .collect()
}

/// Truncations scattered across a long run keep their budget: each
/// complete tool turn starts the chain of [`MAX_CONTINUATIONS`] again, so
/// five cuts in one run each earn a continuation where three in a row
/// would have stopped it.
#[tokio::test]
async fn a_complete_turn_starts_the_continuation_chain_again() {
    let mut responses = Vec::new();
    for piece in ["one ", "two ", "three ", "four ", "five "] {
        responses.push(length_response(piece));
        responses.push(tool_call_response(&[ECHO]));
    }
    responses.push(text_response("done"));
    let server = FakeServer::spawn(responses);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));

    let (result, entries, events) = drive(&server.base_url, registry, "hi").await;
    assert!(result.is_ok(), "{result:?}");

    let values = event_values(&events);
    assert_eq!(
        values.last().unwrap(),
        &json!({"type": "run_ended", "turns": 11}),
        "{values:?}"
    );
    let continued = warned(&values)
        .into_iter()
        .filter(|w| w.contains("continuing from the partial answer"))
        .collect::<Vec<_>>();
    assert_eq!(continued.len(), 5, "{values:?}");
    assert!(
        continued.iter().all(|w| w.ends_with("(1/3)")),
        "{continued:?}"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|e| matches!(
                super::super::messages_of(e).content.as_deref(),
                Some(CONTINUE)
            ))
            .count(),
        5
    );
}

/// Chains of cuts under the per-chain cap, back to back with complete tool
/// turns, still stop at [`MAX_RUN_CONTINUATIONS`] for the run.
#[tokio::test]
async fn the_run_cap_stops_a_run_that_keeps_truncating_on_and_off() {
    let mut responses = Vec::new();
    for _ in 0..MAX_RUN_CONTINUATIONS / MAX_CONTINUATIONS {
        for piece in ["one ", "two ", "three "] {
            responses.push(length_response(piece));
        }
        responses.push(tool_call_response(&[ECHO]));
    }
    responses.push(length_response("one cut too many"));
    let server = FakeServer::spawn(responses);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));

    let (result, _, events) = drive(&server.base_url, registry, "hi").await;
    assert!(result.is_ok(), "{result:?}");

    let values = event_values(&events);
    assert_eq!(
        warned(&values).last().unwrap(),
        &format!(
            "response truncated by max_tokens=512; stopping after {MAX_RUN_CONTINUATIONS} continuation(s) this run — the partial answer stands"
        ),
        "{values:?}"
    );
    assert_eq!(
        values.last().unwrap(),
        &json!({"type": "run_ended", "turns": 17})
    );
    assert_eq!(
        server.requests.lock().unwrap().len(),
        17,
        "no request is made past the run cap"
    );
}

/// A cut stream that carried tool calls is continued with the prose
/// instruction: only a `max_tokens` cut asks for the call in pieces.
#[tokio::test]
async fn a_cut_stream_with_tool_calls_keeps_the_plain_continuation() {
    let server = FakeServer::spawn_sse(vec![
        sse_tool_call_response_cut(&[ECHO]),
        sse_text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));

    let (result, entries, _) = drive_streaming(&server.base_url, registry, "hi", true).await;
    assert!(result.is_ok(), "{result:?}");

    let continued: Vec<&str> = entries
        .iter()
        .map(super::super::messages_of)
        .filter(|m| m.role == crate::agent::session::Role::User)
        .filter_map(|m| m.content.as_deref())
        .collect();
    assert_eq!(continued, vec!["hi", CONTINUE]);
}
