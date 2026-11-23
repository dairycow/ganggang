//! Continuation scripts (GAP 2 + GAP 11): a `length` finish or a stream
//! cut keeps its partial answer and asks for the rest — visibly, a bounded
//! number of times, with the session left replay-safe.

use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::super::fake::{
    FakeServer, length_response, length_tool_call_response, sse_text_response,
    sse_text_response_cut, text_response,
};
use super::super::{
    Echo, Registry, drive, drive_streaming, drive_with_abort, event_values, messages_of,
};
use crate::agent::loop_::cut::{CONTINUE_INSTRUCTION as CONTINUE, MAX_CONTINUATIONS};
use crate::agent::session::Role;

/// A `length` finish is no longer only a warning: the partial answer is
/// kept, exactly one continuation user message rides the next request, and
/// the run ends cleanly on the completed answer.
#[tokio::test]
async fn a_length_finish_continues_from_the_partial_answer() {
    let server = FakeServer::spawn(vec![
        length_response("partial answ"),
        text_response("er, and that is the rest"),
    ]);

    let (result, entries, events) = drive(&server.base_url, Registry::new(), "hi").await;
    assert!(result.is_ok(), "{result:?}");

    let values = event_values(&events);
    // §17: the truncation stays visible, and the warning says what the
    // loop is doing about it — one warning for the short turn, never two.
    assert_eq!(
        values
            .iter()
            .filter(|e| e["type"] == "warned")
            .map(|e| e["message"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["response truncated by max_tokens=512; continuing from the partial answer (1/3)"],
        "{values:?}"
    );
    assert_eq!(
        values.iter().find(|e| e["type"] == "turn_ended").unwrap()["stop_reason"],
        "length",
        "a truncated turn ends with a reason that says so"
    );
    assert_eq!(
        values.last().unwrap(),
        &json!({"type": "run_ended", "turns": 2}),
        "the continuation turn finishes the answer"
    );

    // Two requests: the original, then the one carrying the continuation.
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "exactly one continuation request");
    let contents: Vec<&str> = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .map(|m| m["content"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        contents,
        vec!["hi", CONTINUE],
        "the second request replays the partial plus one continuation message"
    );

    // Session order: user, assistant(partial), user(continuation),
    // assistant(the rest).
    let roles: Vec<Role> = entries.iter().map(|e| messages_of(e).role).collect();
    assert_eq!(
        roles,
        vec![Role::User, Role::Assistant, Role::User, Role::Assistant]
    );
    assert_eq!(
        messages_of(&entries[1]).content.as_deref(),
        Some("partial answ")
    );
    assert_eq!(messages_of(&entries[2]).content.as_deref(), Some(CONTINUE));
    assert_eq!(
        messages_of(&entries[3]).content.as_deref(),
        Some("er, and that is the rest")
    );
}

/// A `length` finish mid-tool-call never executes the call — its arguments
/// can be cut mid-JSON and the answer they belong to is unfinished — but
/// every call still gets a persisted error result, so the replayed session
/// stays valid (§8).
#[tokio::test]
async fn a_length_truncation_never_runs_its_tool_calls_but_stays_replay_safe() {
    let server = FakeServer::spawn(vec![
        length_tool_call_response(&[("call_cut", "echo", r#"{"text":"hal"#)]),
        text_response("carried on without it"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));

    let (result, entries, events) = drive(&server.base_url, registry, "say hi").await;
    assert!(result.is_ok(), "{result:?}");

    let values = event_values(&events);
    assert!(
        !values.iter().any(|e| e["type"] == "tool_call_started"),
        "a truncated call must never dispatch: {values:?}"
    );

    // The synthetic result is persisted, in order, right after the call.
    let assistant = messages_of(&entries[1]);
    assert_eq!(assistant.role, Role::Assistant);
    assert_eq!(assistant.tool_calls[0].id, "call_cut");
    let tool_result = messages_of(&entries[2]);
    assert_eq!(tool_result.role, Role::ToolResult);
    assert_eq!(tool_result.tool_call_id.as_deref(), Some("call_cut"));
    assert!(tool_result.is_error);
    assert_eq!(
        tool_result.content.as_deref(),
        Some("not executed: response was truncated")
    );
    assert_eq!(messages_of(&entries[3]).content.as_deref(), Some(CONTINUE));

    // And the next request really replays the pair: a call with an answer.
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let messages = requests[1]["messages"].as_array().unwrap();
    let assistant = messages
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("the assistant call is replayed");
    assert_eq!(assistant["tool_calls"][0]["id"], "call_cut");
    let tool = messages
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("and its result");
    assert_eq!(tool["tool_call_id"], "call_cut");
    assert_eq!(
        tool["content"], "not executed: response was truncated",
        "the model reads why the call did not run"
    );
}

/// The budget is real: a server that truncates forever earns
/// `MAX_CONTINUATIONS` continuations, one warning each, then the turn ends
/// with the partial answer standing — no unbounded request stream. (The
/// script has exactly `MAX_CONTINUATIONS + 1` responses: the server never
/// accepts another connection, so a runaway loop would fail rather than
/// hang.)
#[tokio::test]
async fn the_continuation_budget_is_bounded_and_then_the_turn_ends() {
    let scripts = vec![
        length_response("one "),
        length_response("two "),
        length_response("three "),
        length_response("four "),
    ];
    let server = FakeServer::spawn(scripts);

    let (result, entries, events) = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        drive(&server.base_url, Registry::new(), "hi"),
    )
    .await
    .expect("the run must end without waiting on another response");
    assert!(
        result.is_ok(),
        "a spent budget ends the turn, it does not fail it"
    );

    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        MAX_CONTINUATIONS as usize + 1,
        "one request per continuation, no more"
    );

    let values = event_values(&events);
    let warnings: Vec<&str> = values
        .iter()
        .filter(|e| e["type"] == "warned")
        .map(|e| e["message"].as_str().unwrap())
        .collect();
    assert_eq!(
        warnings.len(),
        MAX_CONTINUATIONS as usize + 1,
        "one warning per short turn: {warnings:?}"
    );
    assert!(
        warnings.last().unwrap().contains(&format!(
            "stopping after {MAX_CONTINUATIONS} continuation(s)"
        )),
        "the last warning says why it stopped: {warnings:?}"
    );
    assert_eq!(
        values.last().unwrap(),
        &json!({"type": "run_ended", "turns": MAX_CONTINUATIONS + 1}),
        "the run ends cleanly, with every turn counted"
    );
    assert_eq!(
        entries.len(),
        // user + (assistant + continuation user) per continued turn +
        // the last short turn's assistant entry, which gets no answer.
        1 + 2 * MAX_CONTINUATIONS as usize + 1,
        "no continuation message is appended once the budget is spent"
    );
}

/// Esc between turns still works when the loop is about to continue: the
/// continuation is another request, and a request the user has already
/// disowned is not worth making.
#[tokio::test]
async fn a_latched_abort_stops_the_loop_before_a_continuation_request() {
    let abort = Arc::new(AtomicBool::new(false));
    let flag = abort.clone();
    let server = FakeServer::spawn_with_hook(vec![length_response("partial answ")], move |index| {
        if index == 0 {
            flag.store(true, Ordering::Relaxed);
        }
    });

    let drove = drive_with_abort(&server.base_url, Registry::new(), "hi", abort);
    let (result, entries, events) = tokio::time::timeout(std::time::Duration::from_secs(10), drove)
        .await
        .expect("the run must end without making a second request");

    assert!(result.is_ok());
    let values = event_values(&events);
    assert_eq!(values.last().unwrap(), &json!({"type": "run_aborted"}));
    assert_eq!(
        server.requests.lock().unwrap().len(),
        1,
        "no continuation request after the user stopped the run"
    );
    let last = messages_of(entries.last().unwrap());
    assert_eq!(last.role, Role::User);
    assert_eq!(last.content.as_deref(), Some("[aborted by user]"));
}

/// The streaming path salvage, end to end: a body that ends without
/// `[DONE]` no longer aborts the run — the text the user already watched
/// stream in is kept, and the answer is asked for again.
#[tokio::test]
async fn a_cut_stream_continues_the_answer_instead_of_losing_it() {
    let server = FakeServer::spawn_sse(vec![
        sse_text_response_cut("half an"),
        sse_text_response("swer"),
    ]);
    let registry = Registry::new();

    let (result, entries, events) = drive_streaming(&server.base_url, registry, "ask", true).await;
    assert!(
        result.is_ok(),
        "a cut stream is not a failed run: {result:?}"
    );

    let values = event_values(&events);
    assert!(
        values.iter().any(|e| e["type"] == "warned"
            && e["message"]
                .as_str()
                .unwrap()
                .contains("stream ended early; continuing from the partial answer")),
        "{values:?}"
    );
    assert_eq!(
        values.last().unwrap(),
        &json!({"type": "run_ended", "turns": 2})
    );

    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let contents: Vec<&str> = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .map(|m| m["content"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(contents, vec!["ask", CONTINUE]);

    let assistant: Vec<Option<&str>> = entries
        .iter()
        .map(messages_of)
        .filter(|m| m.role == Role::Assistant)
        .map(|m| m.content.as_deref())
        .collect();
    assert_eq!(
        assistant,
        vec![Some("half an"), Some("swer")],
        "the partial survives on its own entry, and the continuation's answer follows it"
    );
}
