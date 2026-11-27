//! Thought-only truncation scripts: a `length` turn that spent the whole
//! `max_tokens` on reasoning has no partial to continue, so the loop asks
//! for the same turn again with reasoning off, visibly. The first turn in
//! a row is retried on its own; the second keeps reasoning off for the
//! rest of the run.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use super::super::fake::{
    FakeServer, sse_text_response, sse_thought_only_length_response, text_response,
    thought_only_length_response, tool_call_response,
};
use super::super::{Echo, Registry, drive, drive_streaming, event_values, messages_of};
use crate::agent::session::{Entry, Role};
use crate::agent::{Agent, hooks::Hooks};
use crate::events::AgentEvent;
use crate::llm::ApiKind;
use crate::llm::client::LlmClient;
use crate::llm::types::Thinking;

/// The warning the first thought-only turn in a row earns. It names the
/// field the retry carries, which asks the endpoint not to reason; whether
/// the endpoint complies is not gg's to know.
const ASKED_AGAIN: &str = "response truncated by max_tokens=512 with nothing but reasoning; asking again with reasoning_effort: \"none\" for this turn";
/// [`ASKED_AGAIN`] on the Anthropic Messages API, whose request has no
/// reasoning field: the retry is the same request again.
const ASKED_AGAIN_SAME_REQUEST: &str = "response truncated by max_tokens=512 with nothing but reasoning; asking again with the same request for this turn";
/// The warning the second thought-only turn back to back earns.
const LATCHED: &str = "response truncated by max_tokens=512 with nothing but reasoning again; asking again with reasoning off, which stays off for the rest of this run";
/// The warning a thought-only turn earns when no retry is left to make.
const STOPPED: &str = "response truncated by max_tokens=512 before producing visible output; stopping instead of continuing";
/// The warning the retry earns when it is thought-only too: the endpoint
/// thought although the request asked it not to.
const RETRY_THOUGHT_ANYWAY: &str = "response truncated by max_tokens=512 with nothing but reasoning; the retry went out with reasoning off and the endpoint thought anyway (512 reasoning tokens); stopping instead of continuing";
/// [`RETRY_THOUGHT_ANYWAY`] for a request that was no retry: the run had
/// switched reasoning off, or the endpoint is configured that way.
const OFF_THOUGHT_ANYWAY: &str = "response truncated by max_tokens=512 with nothing but reasoning; the request went out with reasoning off and the endpoint thought anyway (512 reasoning tokens); stopping instead of continuing";
/// [`RETRY_THOUGHT_ANYWAY`] when the provider reports no reasoning tokens.
const RETRY_THOUGHT_ANYWAY_UNCOUNTED: &str = "response truncated by max_tokens=512 with nothing but reasoning; the retry went out with reasoning off and the endpoint thought anyway; stopping instead of continuing";

fn warned(values: &[Value]) -> Vec<&str> {
    values
        .iter()
        .filter(|e| e["type"] == "warned")
        .filter_map(|e| e["message"].as_str())
        .collect()
}

fn stop_reasons(values: &[Value]) -> Vec<&str> {
    values
        .iter()
        .filter(|e| e["type"] == "turn_ended")
        .filter_map(|e| e["stop_reason"].as_str())
        .collect()
}

/// The agent every script here drives: the endpoint the client describes,
/// and the given turn budget.
fn agent(registry: Registry, client: LlmClient, max_turns: Option<u32>) -> Agent {
    Agent::new(
        client,
        registry,
        "fake-model".into(),
        512,
        Arc::new(AtomicBool::new(false)),
        std::env::temp_dir(),
        None,
        false,
        131_072,
        Hooks::default(),
    )
    .with_max_turns(max_turns)
}

/// [`drive`](super::super::drive) with an endpoint and a turn budget of
/// the caller's own.
async fn drive_with(
    registry: Registry,
    client: LlmClient,
    max_turns: Option<u32>,
) -> (anyhow::Result<()>, Vec<Entry>, Vec<AgentEvent>) {
    let mut agent = agent(registry, client, max_turns);
    let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
    let handle = tokio::spawn(async move {
        let result = agent.run("hi".to_string(), &mut tx).await;
        (result, agent.entries().to_vec())
    });
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    let (result, entries) = handle.await.unwrap();
    (result, entries, events)
}

/// One whole raw HTTP response, as a server writes it.
fn raw(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// A thought-only `length` turn in Anthropic Messages shape: a `thinking`
/// block, then the `max_tokens` stop. The API reports no reasoning token
/// count of its own.
fn anthropic_thought_only_response(reasoning: &str) -> String {
    json!({
        "type": "message",
        "content": [{"type": "thinking", "thinking": reasoning}],
        "stop_reason": "max_tokens",
        "usage": {"input_tokens": 10, "output_tokens": 512},
    })
    .to_string()
}

/// An endpoint with a `thinking_off` switch sends it only on the retry:
/// the first request carries the endpoint's settings, the retry carries
/// `"thinking": {"type": "disabled"}` beside `reasoning_effort: "none"`.
#[tokio::test]
async fn a_thinking_off_endpoints_retry_carries_the_switch() {
    let server = FakeServer::spawn(vec![
        thought_only_length_response("thinking"),
        text_response("the answer"),
    ]);
    let client = LlmClient::new(&server.base_url)
        .unwrap()
        .with_thinking_off(Some(Thinking::of("disabled")));

    let (result, _, events) = drive_with(Registry::new(), client, None).await;
    assert!(result.is_ok(), "{result:?}");

    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0].get("thinking").is_none(),
        "the first request carries the endpoint's settings: {}",
        requests[0]
    );
    assert_eq!(requests[1]["reasoning_effort"], "none");
    assert_eq!(requests[1]["thinking"], json!({"type": "disabled"}));
    assert_eq!(warned(&event_values(&events)), vec![ASKED_AGAIN]);
    assert_eq!(
        event_values(&events).last().unwrap(),
        &json!({"type": "run_ended", "turns": 2})
    );
}

/// The turn is asked once more with `reasoning_effort: "none"`, over the
/// same messages. Nothing of the thought-only turn is kept, and the run
/// ends on the answer.
#[tokio::test]
async fn a_thought_only_truncation_is_asked_again_without_reasoning() {
    let server = FakeServer::spawn(vec![
        thought_only_length_response("thinking, always thinking"),
        text_response("the answer"),
    ]);

    let (result, entries, events) = drive(&server.base_url, Registry::new(), "hi").await;
    assert!(result.is_ok(), "{result:?}");

    let values = event_values(&events);
    assert_eq!(warned(&values), vec![ASKED_AGAIN], "{values:?}");
    assert_eq!(
        stop_reasons(&values),
        vec!["length", "stop"],
        "the cut names itself, then the retry answers"
    );
    assert_eq!(
        values.last().unwrap(),
        &json!({"type": "run_ended", "turns": 2})
    );

    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "exactly one retry");
    assert!(
        requests[0].get("reasoning_effort").is_none(),
        "the first request carries the endpoint's settings: {}",
        requests[0]
    );
    assert_eq!(requests[1]["reasoning_effort"], "none");
    assert!(
        requests[1].get("reasoning_budget_tokens").is_none(),
        "{}",
        requests[1]
    );
    assert_eq!(
        requests[0]["messages"], requests[1]["messages"],
        "the retry asks the same question"
    );

    let roles: Vec<Role> = entries.iter().map(|e| messages_of(e).role).collect();
    assert_eq!(
        roles,
        vec![Role::User, Role::Assistant],
        "no thought-only entry: {entries:?}"
    );
    assert_eq!(
        messages_of(&entries[1]).content.as_deref(),
        Some("the answer")
    );
}

/// The streaming path sends the same retry.
#[tokio::test]
async fn a_streamed_thought_only_truncation_is_asked_again_without_reasoning() {
    let server = FakeServer::spawn_sse(vec![
        sse_thought_only_length_response("thinking"),
        sse_text_response("the answer"),
    ]);

    let (result, entries, events) =
        drive_streaming(&server.base_url, Registry::new(), "hi", true).await;
    assert!(result.is_ok(), "{result:?}");

    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["stream"], true);
    assert!(requests[0].get("reasoning_effort").is_none());
    assert_eq!(requests[1]["reasoning_effort"], "none");
    assert_eq!(warned(&event_values(&events)), vec![ASKED_AGAIN]);
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(
        messages_of(&entries[1]).content.as_deref(),
        Some("the answer")
    );
}

/// The retry is scoped to its turn: the tool turn after it, and the
/// answer after that, both go out with the endpoint's settings again.
#[tokio::test]
async fn reasoning_returns_to_the_configured_effort_after_one_thought_only_turn() {
    let server = FakeServer::spawn(vec![
        thought_only_length_response("thinking"),
        tool_call_response(&[("call_1", "echo", r#"{"text":"x"}"#)]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));

    let (result, _, events) = drive(&server.base_url, registry, "hi").await;
    assert!(result.is_ok(), "{result:?}");

    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].get("reasoning_effort").is_none());
    assert_eq!(requests[1]["reasoning_effort"], "none");
    assert!(
        requests[2].get("reasoning_effort").is_none(),
        "the turn after the retry is back as configured: {}",
        requests[2]
    );
    assert_eq!(warned(&event_values(&events)), vec![ASKED_AGAIN]);
    assert_eq!(
        event_values(&events).last().unwrap(),
        &json!({"type": "run_ended", "turns": 3})
    );
}

/// Two thought-only turns back to back: the second retry latches, and
/// every request after it in the run goes out with `none`.
#[tokio::test]
async fn two_thought_only_turns_in_a_row_keep_reasoning_off_for_the_run() {
    let server = FakeServer::spawn(vec![
        thought_only_length_response("thinking"),
        tool_call_response(&[("call_1", "echo", r#"{"text":"x"}"#)]),
        thought_only_length_response("still thinking"),
        tool_call_response(&[("call_2", "echo", r#"{"text":"y"}"#)]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));

    let (result, _, events) = drive(&server.base_url, registry, "hi").await;
    assert!(result.is_ok(), "{result:?}");

    let requests = server.requests.lock().unwrap().clone();
    let efforts: Vec<String> = requests
        .iter()
        .map(|r| {
            r.get("reasoning_effort").map_or("configured".into(), |v| {
                v.as_str().unwrap_or_default().to_string()
            })
        })
        .collect();
    assert_eq!(
        efforts,
        vec!["configured", "none", "configured", "none", "none"],
        "the turn between the two cuts goes as configured; the latch holds after the second"
    );
    assert_eq!(
        warned(&event_values(&events)),
        vec![ASKED_AGAIN, LATCHED],
        "{events:?}"
    );
    assert_eq!(
        event_values(&events).last().unwrap(),
        &json!({"type": "run_ended", "turns": 5})
    );
}

/// A complete turn that needed no retry ends the run of thought-only
/// cuts: the next one is a first again, retried on its own.
#[tokio::test]
async fn a_complete_turn_between_thought_only_cuts_starts_the_count_again() {
    let server = FakeServer::spawn(vec![
        thought_only_length_response("thinking"),
        tool_call_response(&[("call_1", "echo", r#"{"text":"x"}"#)]),
        tool_call_response(&[("call_2", "echo", r#"{"text":"y"}"#)]),
        thought_only_length_response("thinking"),
        tool_call_response(&[("call_3", "echo", r#"{"text":"z"}"#)]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));

    let (result, _, events) = drive(&server.base_url, registry, "hi").await;
    assert!(result.is_ok(), "{result:?}");

    let requests = server.requests.lock().unwrap().clone();
    let efforts: Vec<String> = requests
        .iter()
        .map(|r| {
            r.get("reasoning_effort").map_or("configured".into(), |v| {
                v.as_str().unwrap_or_default().to_string()
            })
        })
        .collect();
    assert_eq!(
        efforts,
        vec![
            "configured",
            "none",
            "configured",
            "configured",
            "none",
            "configured"
        ],
        "neither retry latches: {efforts:?}"
    );
    assert_eq!(
        warned(&event_values(&events)),
        vec![ASKED_AGAIN, ASKED_AGAIN],
        "{events:?}"
    );
}

/// A retry that is thought-only too has nothing left to switch off: the
/// run stops the way a thought-only turn with reasoning already off does,
/// and the warning says the endpoint thought although the retry asked it
/// not to.
#[tokio::test]
async fn a_retry_that_is_thought_only_again_ends_the_run() {
    let server = FakeServer::spawn(vec![
        thought_only_length_response("thinking"),
        thought_only_length_response("thinking, even with reasoning off"),
    ]);

    let (result, _, events) = drive(&server.base_url, Registry::new(), "hi").await;
    assert!(result.is_ok(), "{result:?}");

    let values = event_values(&events);
    assert_eq!(
        warned(&values),
        vec![ASKED_AGAIN, RETRY_THOUGHT_ANYWAY],
        "{values:?}"
    );
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        2,
        "the same request is not made a third time"
    );
    assert_eq!(
        requests[1]["reasoning_effort"], "none",
        "the warning's claim is the retry's own bytes: {}",
        requests[1]
    );
    assert_eq!(
        values.last().unwrap(),
        &json!({"type": "run_ended", "turns": 2})
    );
}

/// A provider that reports no reasoning token count gets the same warning
/// without one.
#[tokio::test]
async fn a_streamed_retry_that_is_thought_only_warns_without_a_token_count() {
    let server = FakeServer::spawn_sse(vec![
        sse_thought_only_length_response("thinking"),
        sse_thought_only_length_response("thinking, even with reasoning off"),
    ]);

    let (result, _, events) = drive_streaming(&server.base_url, Registry::new(), "hi", true).await;
    assert!(result.is_ok(), "{result:?}");

    assert_eq!(
        warned(&event_values(&events)),
        vec![ASKED_AGAIN, RETRY_THOUGHT_ANYWAY_UNCOUNTED]
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

/// The Anthropic Messages request has no reasoning field, so the retry is
/// the same request again: the warning says that, and says nothing about
/// reasoning going out off.
#[tokio::test]
async fn an_anthropic_retry_is_warned_about_as_the_same_request() {
    let server = FakeServer::spawn(vec![
        anthropic_thought_only_response("thinking"),
        anthropic_thought_only_response("thinking, again"),
    ]);
    let client = LlmClient::new(&server.base_url)
        .unwrap()
        .with_api(ApiKind::AnthropicMessages);

    let (result, _, events) = drive_with(Registry::new(), client, None).await;
    assert!(result.is_ok(), "{result:?}");

    let values = event_values(&events);
    assert_eq!(
        warned(&values),
        vec![ASKED_AGAIN_SAME_REQUEST, STOPPED],
        "{values:?}"
    );
    assert_eq!(
        values.last().unwrap(),
        &json!({"type": "run_ended", "turns": 2})
    );
}

/// Once reasoning is off for the run, a thought-only turn is no retry: the
/// warning names the request, not a retry, and the run ends there.
#[tokio::test]
async fn a_thought_only_turn_with_reasoning_off_for_the_run_names_the_request() {
    let server = FakeServer::spawn(vec![
        thought_only_length_response("thinking"),
        tool_call_response(&[("call_1", "echo", r#"{"text":"x"}"#)]),
        thought_only_length_response("still thinking"),
        tool_call_response(&[("call_2", "echo", r#"{"text":"y"}"#)]),
        thought_only_length_response("thinking, with reasoning off"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));

    let (result, _, events) = drive(&server.base_url, registry, "hi").await;
    assert!(result.is_ok(), "{result:?}");

    let values = event_values(&events);
    assert_eq!(
        warned(&values),
        vec![ASKED_AGAIN, LATCHED, OFF_THOUGHT_ANYWAY],
        "{values:?}"
    );
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        5,
        "the thought-only turn with reasoning off ends the run"
    );
    assert_eq!(requests[4]["reasoning_effort"], "none", "{}", requests[4]);
    assert_eq!(
        values.last().unwrap(),
        &json!({"type": "run_ended", "turns": 5})
    );
}

/// The run is the scope of the latch: the next run starts with the
/// endpoint's settings.
#[tokio::test]
async fn the_next_run_asks_with_the_configured_reasoning_again() {
    let server = FakeServer::spawn(vec![
        thought_only_length_response("thinking"),
        tool_call_response(&[("call_1", "echo", r#"{"text":"x"}"#)]),
        thought_only_length_response("thinking"),
        tool_call_response(&[("call_2", "echo", r#"{"text":"y"}"#)]),
        text_response("first"),
        tool_call_response(&[("call_3", "echo", r#"{"text":"z"}"#)]),
        text_response("second"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));
    let client = LlmClient::new(&server.base_url).unwrap();
    let mut agent = agent(registry, client, None);

    for input in ["one", "two"] {
        let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
        agent.run(input.to_string(), &mut tx).await.unwrap();
        drop(tx);
        while rx.recv().await.is_some() {}
    }

    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 7);
    assert!(requests[0].get("reasoning_effort").is_none());
    assert_eq!(requests[1]["reasoning_effort"], "none");
    assert!(requests[2].get("reasoning_effort").is_none());
    assert_eq!(requests[3]["reasoning_effort"], "none");
    assert_eq!(requests[4]["reasoning_effort"], "none");
    assert!(
        requests[5].get("reasoning_effort").is_none(),
        "the second run starts as configured: {}",
        requests[5]
    );
    assert!(requests[6].get("reasoning_effort").is_none());
}

/// An endpoint already on `none` has nothing to switch off: a model that
/// thinks anyway gets the warning that says so, with no retry, and its
/// thought-only entry stays.
#[tokio::test]
async fn a_truncation_with_reasoning_already_off_is_not_asked_again() {
    let server = FakeServer::spawn(vec![thought_only_length_response("thinking")]);
    let client = LlmClient::new(&server.base_url)
        .unwrap()
        .with_reasoning_effort(Some("none".into()));

    let (result, entries, events) = drive_with(Registry::new(), client, None).await;
    assert!(result.is_ok(), "{result:?}");

    let values = event_values(&events);
    assert_eq!(warned(&values), vec![OFF_THOUGHT_ANYWAY], "{values:?}");
    assert_eq!(
        values.last().unwrap(),
        &json!({"type": "run_ended", "turns": 1})
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(messages_of(&entries[1]).role, Role::Assistant);
}

/// The retry is a turn, so it is not made on the run's last one: there the
/// run stops the way it would without the retry.
#[tokio::test]
async fn a_truncation_on_the_last_turn_is_not_asked_again() {
    let server = FakeServer::spawn(vec![thought_only_length_response("thinking")]);
    let client = LlmClient::new(&server.base_url).unwrap();

    let (result, _, events) = drive_with(Registry::new(), client, Some(1)).await;
    assert!(result.is_ok(), "{result:?}");

    assert_eq!(warned(&event_values(&events)), vec![STOPPED]);
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

/// A server that refuses `reasoning_effort: "none"` ends the run the way
/// the truncation alone would have: a clean finish whose warning quotes the
/// refusal, not a failed run and not another request.
#[tokio::test]
async fn a_refused_retry_ends_the_run_with_the_server_answer() {
    let server = FakeServer::spawn_raw(vec![
        raw("200 OK", &thought_only_length_response("thinking")),
        raw(
            "400 Bad Request",
            r#"{"error":{"message":"Invalid value: 'none'"}}"#,
        ),
    ]);

    let (result, _, events) = drive(&server.base_url, Registry::new(), "hi").await;
    assert!(result.is_ok(), "{result:?}");

    let values = event_values(&events);
    let warnings = warned(&values);
    assert_eq!(warnings.len(), 2, "{values:?}");
    assert_eq!(warnings[0], ASKED_AGAIN);
    assert!(
        warnings[1].starts_with("the request with reasoning off was refused; stopping instead: "),
        "{}",
        warnings[1]
    );
    assert!(
        warnings[1].contains("400 Bad Request") && warnings[1].contains("Invalid value: 'none'"),
        "the server's own words: {}",
        warnings[1]
    );
    assert_eq!(
        values.last().unwrap(),
        &json!({"type": "run_ended", "turns": 2})
    );
    assert_eq!(
        server.requests.lock().unwrap().len(),
        2,
        "a refusal is not retried"
    );
}
