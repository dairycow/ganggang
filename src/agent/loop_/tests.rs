//! Scripted-model tests for the loop: canned responses from [`fake`],
//! requests recorded and replay-asserted.

mod compact;
mod edge;
mod fake;
mod hooks;
mod wave23;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use crate::agent::Agent;
use crate::agent::session::{Entry, Message, Role};
use crate::agent::tools::{self, Registry, Tool, ToolResult, parse_args};
use crate::events::AgentEvent;
use crate::llm::client::LlmClient;

use fake::{
    FakeServer, sse_reasoning_text_response, sse_text_response, sse_tool_call_response,
    text_response, tool_call_response,
};

// ------------------------------------------------------------- test tools

pub(super) struct Echo;

#[derive(serde::Deserialize)]
struct EchoArgs {
    text: String,
}

impl Tool for Echo {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn description(&self) -> &'static str {
        "echo the text back"
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
    }
    fn execute<'a>(&'a self, args: &'a Value) -> tools::BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            match parse_args::<EchoArgs>(args) {
                Ok(a) => ToolResult::ok(a.text),
                Err(e) => ToolResult::err(e),
            }
        })
    }
}

/// Sets the abort flag while running, so mid-batch aborts are deterministic
/// (no race between the drain loop observing events and the loop checking).
pub(super) struct SetFlagThenEcho(Arc<AtomicBool>);

impl Tool for SetFlagThenEcho {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn description(&self) -> &'static str {
        "echo the text back"
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
    }
    fn execute<'a>(&'a self, args: &'a Value) -> tools::BoxFuture<'a, ToolResult> {
        let flag = self.0.clone();
        Box::pin(async move {
            flag.store(true, Ordering::Relaxed);
            match parse_args::<EchoArgs>(args) {
                Ok(a) => ToolResult::ok(a.text),
                Err(e) => ToolResult::err(e),
            }
        })
    }
}

/// A tool whose observable side effect is a file — proof it ran.
pub(super) struct Touch(PathBuf);

impl Tool for Touch {
    fn name(&self) -> &'static str {
        "touch"
    }
    fn description(&self) -> &'static str {
        "create the marker file"
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn execute<'a>(&'a self, _args: &'a Value) -> tools::BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            std::fs::write(&self.0, "ran")
                .map_err(|e| e.to_string())
                .map_or_else(ToolResult::err, |_| ToolResult::ok("touched".into()))
        })
    }
}

// ------------------------------------------------------------------ harness

pub(super) async fn drive(
    base_url: &str,
    registry: Registry,
    input: &str,
) -> (anyhow::Result<()>, Vec<Entry>, Vec<AgentEvent>) {
    drive_full(
        base_url,
        registry,
        input,
        Arc::new(AtomicBool::new(false)),
        None,
        false,
        Default::default(),
    )
    .await
}

/// Same as [`drive`], with a loaded hook set for §22 scripts.
pub(super) async fn drive_hooks(
    base_url: &str,
    registry: Registry,
    input: &str,
    hooks: crate::agent::hooks::Hooks,
) -> (anyhow::Result<()>, Vec<Entry>, Vec<AgentEvent>) {
    drive_full(
        base_url,
        registry,
        input,
        Arc::new(AtomicBool::new(false)),
        None,
        false,
        hooks,
    )
    .await
}

pub(super) async fn drive_with_abort(
    base_url: &str,
    registry: Registry,
    input: &str,
    abort: Arc<AtomicBool>,
) -> (anyhow::Result<()>, Vec<Entry>, Vec<AgentEvent>) {
    drive_full(
        base_url,
        registry,
        input,
        abort,
        None,
        false,
        Default::default(),
    )
    .await
}

/// Same as [`drive_with_abort`], but persisting into `store` so the
/// write-through path is exercised end to end.
pub(super) async fn drive_with_store(
    base_url: &str,
    registry: Registry,
    input: &str,
    abort: Arc<AtomicBool>,
    store: Option<crate::agent::session::io::SessionStore>,
) -> (anyhow::Result<()>, Vec<Entry>, Vec<AgentEvent>) {
    drive_full(
        base_url,
        registry,
        input,
        abort,
        store,
        false,
        Default::default(),
    )
    .await
}

/// The streaming-path variant (M5): the agent requests SSE chunks.
pub(super) async fn drive_streaming(
    base_url: &str,
    registry: Registry,
    input: &str,
    stream: bool,
) -> (anyhow::Result<()>, Vec<Entry>, Vec<AgentEvent>) {
    drive_full(
        base_url,
        registry,
        input,
        Arc::new(AtomicBool::new(false)),
        None,
        stream,
        Default::default(),
    )
    .await
}

/// The harness every script drives: `stream` picks the SSE or plain-JSON
/// request path (real use streams; the scripted tests keep the simpler
/// non-streaming fake unless they are about streaming itself), `store`
/// the session persistence.
#[allow(clippy::too_many_arguments)]
pub(super) async fn drive_full(
    base_url: &str,
    registry: Registry,
    input: &str,
    abort: Arc<AtomicBool>,
    store: Option<crate::agent::session::io::SessionStore>,
    stream: bool,
    hooks: crate::agent::hooks::Hooks,
) -> (anyhow::Result<()>, Vec<Entry>, Vec<AgentEvent>) {
    let client = LlmClient::new(base_url).unwrap();
    let mut agent = Agent::new(
        client,
        registry,
        "fake-model".into(),
        512,
        abort,
        std::env::temp_dir(),
        store,
        stream,
        131_072,
        hooks,
    );
    let input = input.to_string();
    let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
    let handle = tokio::spawn(async move {
        let result = agent.run(input, &mut tx).await;
        (result, agent.entries().to_vec())
    });
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    let (result, entries) = handle.await.unwrap();
    (result, entries, events)
}

pub(super) fn event_values(events: &[AgentEvent]) -> Vec<Value> {
    events
        .iter()
        .map(|e| serde_json::to_value(e).unwrap())
        .collect()
}

pub(super) fn messages_of(entry: &Entry) -> &Message {
    match entry {
        Entry::Message { message, .. } => message,
        Entry::Compaction { .. } => unreachable!("compaction is M6; no entries here"),
    }
}

// ------------------------------------------------------------------- scripts

#[tokio::test]
async fn plain_answer_is_one_turn() {
    let server = FakeServer::spawn(vec![text_response("hello there")]);

    let (result, entries, events) = drive(&server.base_url, Registry::new(), "hi").await;

    assert!(result.is_ok());
    assert_eq!(
        event_values(&events),
        vec![
            json!({"type": "run_started"}),
            json!({"type": "turn_started"}),
            json!({"type": "assistant_text", "delta": "hello there"}),
            json!({"type": "turn_ended", "stop_reason": "stop", "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}),
            json!({"type": "run_ended", "turns": 1}),
        ]
    );
    assert_eq!(entries.len(), 2);
    assert_eq!(messages_of(&entries[0]).role, Role::User);
    assert_eq!(
        messages_of(&entries[1]).content.as_deref(),
        Some("hello there")
    );
}

#[tokio::test]
async fn tool_call_round_trip_replays_in_second_request() {
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("call_1", "echo", r#"{"text":"hi"}"#)]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));

    let (result, entries, events) = drive(&server.base_url, registry, "say hi back").await;

    assert!(result.is_ok());
    assert_eq!(
        event_values(&events),
        vec![
            json!({"type": "run_started"}),
            json!({"type": "turn_started"}),
            json!({"type": "tool_call_started", "name": "echo", "id": "call_1", "args": {"text": "hi"}}),
            json!({"type": "tool_call_finished", "name": "echo", "id": "call_1", "output": "hi", "is_error": false}),
            json!({"type": "turn_ended", "stop_reason": "tool_calls", "usage": {"prompt_tokens": 20, "completion_tokens": 8, "total_tokens": 28}}),
            json!({"type": "turn_started"}),
            json!({"type": "assistant_text", "delta": "done"}),
            json!({"type": "turn_ended", "stop_reason": "stop", "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}),
            json!({"type": "run_ended", "turns": 2}),
        ]
    );

    // Session: user, assistant(call), tool_result, assistant(answer).
    assert_eq!(entries.len(), 4);
    let assistant = messages_of(&entries[1]);
    assert_eq!(assistant.role, Role::Assistant);
    assert_eq!(assistant.usage.as_ref().unwrap().total_tokens, 28);
    let tool_result = messages_of(&entries[2]);
    assert_eq!(tool_result.role, Role::ToolResult);
    assert_eq!(tool_result.tool_call_id.as_deref(), Some("call_1"));
    assert!(!tool_result.is_error);

    // The second request led with a system prompt, then replayed both the
    // assistant call and its result. Positions by role, not index — the
    // system message owns [0].
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let replayed = &requests[1]["messages"];
    assert_eq!(replayed[0]["role"], "system");
    assert!(
        replayed[0]["content"]
            .as_str()
            .unwrap()
            .contains("You are gg")
    );
    let assistant = replayed
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "assistant")
        .unwrap();
    assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
    assert_eq!(assistant["tool_calls"][0]["function"]["name"], "echo");
    let tool = replayed
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    assert_eq!(tool["tool_call_id"], "call_1");
    assert_eq!(tool["content"], "hi");
}

#[tokio::test]
async fn tool_error_recovers_and_completes() {
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("call_1", "ghost", r#"{}"#)]),
        text_response("recovered"),
    ]);

    let (result, entries, events) =
        drive(&server.base_url, Registry::new(), "use a ghost tool").await;

    assert!(result.is_ok());
    let finished = event_values(&events)
        .into_iter()
        .find(|e| e["type"] == "tool_call_finished")
        .unwrap();
    assert_eq!(finished["is_error"], true);
    assert!(
        finished["output"]
            .as_str()
            .unwrap()
            .contains("unknown tool: ghost")
    );
    assert_eq!(
        event_values(&events).last().unwrap(),
        &json!({"type": "run_ended", "turns": 2})
    );

    let tool_result = messages_of(&entries[2]);
    assert_eq!(tool_result.role, Role::ToolResult);
    assert!(tool_result.is_error);
    assert!(
        tool_result
            .content
            .as_deref()
            .unwrap()
            .contains("unknown tool")
    );
}

/// The same scripted conversation, streamed and not: the event streams
/// differ (the answer arrives as deltas, the tool-call turn emits no
/// text events), but the recorded session must be identical — replay
/// cannot depend on how the reply was delivered.
#[tokio::test]
async fn streamed_run_emits_deltas_and_records_the_same_session() {
    let echo_registry = || {
        let mut registry = Registry::new();
        registry.register(Box::new(Echo));
        registry
    };
    let streamed = FakeServer::spawn_sse(vec![
        sse_tool_call_response(&[("call_1", "echo", r#"{"text":"hi"}"#)]),
        sse_text_response("done"),
    ]);
    let plain = FakeServer::spawn(vec![
        tool_call_response(&[("call_1", "echo", r#"{"text":"hi"}"#)]),
        text_response("done"),
    ]);

    let (result, entries, events) =
        drive_streaming(&streamed.base_url, echo_registry(), "say hi back", true).await;
    let (plain_result, plain_entries, _) =
        drive(&plain.base_url, echo_registry(), "say hi back").await;

    assert!(result.is_ok() && plain_result.is_ok());

    // The streamed turn split its answer into two ordered deltas, and the
    // tool-call turn emitted no text events at all.
    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AssistantText { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, vec!["do", "ne"]);
    assert_eq!(
        event_values(&events),
        vec![
            json!({"type": "run_started"}),
            json!({"type": "turn_started"}),
            json!({"type": "tool_call_started", "name": "echo", "id": "call_1", "args": {"text": "hi"}}),
            json!({"type": "tool_call_finished", "name": "echo", "id": "call_1", "output": "hi", "is_error": false}),
            json!({"type": "turn_ended", "stop_reason": "tool_calls", "usage": {"prompt_tokens": 20, "completion_tokens": 8, "total_tokens": 28}}),
            json!({"type": "turn_started"}),
            json!({"type": "assistant_text", "delta": "do"}),
            json!({"type": "assistant_text", "delta": "ne"}),
            json!({"type": "turn_ended", "stop_reason": "stop", "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}),
            json!({"type": "run_ended", "turns": 2}),
        ]
    );

    // Sessions match what the non-streaming script recorded: same wire
    // replay, same usage on the assistant entries.
    assert_eq!(
        crate::agent::session::to_wire(&entries),
        crate::agent::session::to_wire(&plain_entries)
    );
    let usage_of = |entries: &Vec<Entry>| {
        entries
            .iter()
            .map(messages_of)
            .filter(|m| m.role == Role::Assistant)
            .map(|m| {
                m.usage
                    .as_ref()
                    .map(|u| (u.prompt_tokens, u.completion_tokens, u.total_tokens))
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(usage_of(&entries), usage_of(&plain_entries));

    // The request really was a streaming one.
    let requests = streamed.requests.lock().unwrap().clone();
    assert_eq!(requests[0]["stream"], true);
    assert_eq!(requests[0]["stream_options"]["include_usage"], true);
}

/// Thinking rides the stream (M7.3): `Thinking` events precede the text
/// deltas in wire order, the assembled reasoning lands once on the
/// assistant entry, and replay never sends it back to the model.
#[tokio::test]
async fn streamed_thinking_precedes_text_and_records_without_replay() {
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));
    let server = FakeServer::spawn_sse(vec![sse_reasoning_text_response(
        "must answer politely",
        "hello there",
    )]);

    let (result, entries, events) =
        drive_streaming(&server.base_url, registry, "greet me", true).await;
    assert!(result.is_ok(), "{result:?}");

    let order: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Thinking { delta } => Some(format!("thinking:{delta}")),
            AgentEvent::AssistantText { delta } => Some(format!("text:{delta}")),
            _ => None,
        })
        .collect();
    assert_eq!(
        order,
        vec![
            "thinking:must answe",
            "thinking:r politely",
            "text:hello",
            "text: there",
        ]
    );

    // Recorded once, on the assistant entry.
    let assistant = entries
        .iter()
        .map(messages_of)
        .find(|m| m.role == Role::Assistant)
        .expect("an assistant entry");
    assert_eq!(
        assistant.reasoning.as_deref(),
        Some("must answer politely"),
        "assembled thinking persists on the entry"
    );
    // And replay keeps it to the human: no reasoning_content goes out.
    let wire = crate::agent::session::to_wire(&entries);
    assert!(wire.iter().all(|m| m.reasoning_content.is_none()));
    let requests = server.requests.lock().unwrap().clone();
    let sent = requests[0]["messages"].to_string();
    assert!(
        !sent.contains("must answer politely"),
        "thinking must not be sent to the model: {sent}"
    );
}
