//! The wall-clock budget (`--max-seconds`): the 80% nudge rides the next
//! request without rewriting history, the final request forbids tools, and
//! the run stops when that turn is over, whether the model answered or
//! asked for tools anyway.
//!
//! The clock is scripted: a test's `echo` tool moves the run's clock
//! forward while it runs, so no test waits on the wall clock.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::{Value, json};

use super::super::fake::{FakeServer, length_response, text_response, tool_call_response};
use super::super::{Echo, Registry, event_values};
use crate::agent::Agent;
use crate::agent::session::{Entry, Role};
use crate::agent::tools::{BoxFuture, Cancel, Tool, ToolResult, parse_args};
use crate::llm::client::LlmClient;

const CALL: [(&str, &str, &str); 1] = [("c1", "echo", r#"{"text":"t1"}"#)];

#[derive(serde::Deserialize)]
struct EchoArgs {
    text: String,
}

/// `echo` that first moves the run's clock `to` seconds ahead.
struct Tick(Arc<AtomicU64>, u64);

impl Tool for Tick {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn description(&self) -> &'static str {
        "echo the text back"
    }
    fn schema(&self) -> Value {
        Echo.schema()
    }
    fn execute<'a>(&'a self, args: &'a Value, _cancel: &'a Cancel) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            self.0.store(self.1, Ordering::SeqCst);
            match parse_args::<EchoArgs>(args) {
                Ok(a) => ToolResult::ok(a.text),
                Err(e) => ToolResult::err(e),
            }
        })
    }
}

/// Drive a run on a scripted `echo` server with a budget of `limit`
/// seconds, its clock starting `ahead` seconds in. With `tick`, each
/// `echo` call moves the clock to that many seconds into the run. Returns
/// the events, the recorded requests, the agent and its clock.
async fn run(
    responses: Vec<String>,
    limit: u64,
    ahead: u64,
    tick: Option<u64>,
) -> (Vec<Value>, Vec<Value>, Agent, Arc<AtomicU64>) {
    let server = FakeServer::spawn(responses);
    let clock = Arc::new(AtomicU64::new(ahead));
    let echo: Box<dyn Tool> = match tick {
        Some(to) => Box::new(Tick(Arc::clone(&clock), to)),
        None => Box::new(Echo),
    };
    let mut registry = Registry::new();
    registry.register(echo);
    let mut agent = Agent::new(
        LlmClient::new(&server.base_url).unwrap(),
        registry,
        "fake-model".into(),
        512,
        Arc::new(AtomicBool::new(false)),
        std::env::temp_dir(),
        None,
        false,
        131_072,
        Default::default(),
    )
    .with_max_seconds(Some(limit));
    agent.time_ahead = Arc::clone(&clock);

    let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
    agent.run("go".into(), &mut tx).await.unwrap();
    drop(tx);
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    (
        event_values(&events),
        server.requests.lock().unwrap().clone(),
        agent,
        clock,
    )
}

fn warned(values: &[Value]) -> Vec<&str> {
    values
        .iter()
        .filter(|e| e["type"] == "warned")
        .filter_map(|e| e["message"].as_str())
        .collect()
}

fn last_message(request: &Value) -> &Value {
    request["messages"].as_array().unwrap().last().unwrap()
}

fn content_of(message: &Value) -> &str {
    message["content"].as_str().unwrap()
}

fn tool_results(agent: &Agent) -> Vec<String> {
    agent
        .entries()
        .iter()
        .filter_map(|e| match e {
            Entry::Message { message, .. } if message.role == Role::ToolResult => {
                message.content.clone()
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn the_soft_line_nudges_once_without_rewriting_the_request() {
    // The first batch runs the clock to the 80% line.
    let (values, requests, agent, _) = run(
        vec![
            tool_call_response(&CALL),
            tool_call_response(&CALL),
            text_response("done"),
        ],
        1000,
        0,
        Some(800),
    )
    .await;

    assert_eq!(
        warned(&values),
        vec!["approaching max_seconds (800s/1000s); nudging model to wrap up"],
        "{values:?}"
    );
    assert_eq!(
        values.last(),
        Some(&json!({"type": "run_ended", "turns": 3})),
        "a nudge is not a stop"
    );

    // The notice rides the tool result of the turn before it, and stays on
    // that message in the request after; nothing lands in the session.
    let soft = "you have used 800s of a 1000s time budget";
    assert!(!content_of(last_message(&requests[0])).contains(soft));
    assert_eq!(last_message(&requests[1])["role"], "tool");
    assert!(content_of(last_message(&requests[1])).contains(soft));
    // In the request after, the notice is still on that same message:
    // the request grew by appending, so the cached prefix is intact.
    let carried = requests[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(content_of)
        .collect::<Vec<_>>();
    assert_eq!(carried.len(), 2);
    assert!(carried[0].contains(soft), "{carried:?}");
    assert!(!carried[1].contains(soft), "the notice is not repeated");
    assert!(
        !agent
            .entries()
            .iter()
            .any(|e| format!("{e:?}").contains("time budget"))
    );
}

#[tokio::test]
async fn a_final_turn_that_answers_ends_the_run() {
    // The first batch runs the clock out: the second request is the final
    // one, and its answer ends the run.
    let (values, requests, _, clock) = run(
        vec![tool_call_response(&CALL), text_response("done")],
        10,
        0,
        Some(10),
    )
    .await;

    assert_eq!(
        warned(&values),
        vec![
            "reached max_seconds (10s); asking model for final response",
            "reached max_seconds (10s); stopping run",
        ],
        "{values:?}"
    );
    assert_eq!(requests.len(), 2);
    let last = last_message(&requests[1]);
    assert_eq!(last["role"], "tool", "the notice rides the tool result");
    assert!(content_of(last).contains(
        "[system notice: time budget (10s) reached. Do not call any tools; provide your final response now.]"
    ));
    // The stop comes before the turn's own `TurnEnded`, inside the run.
    let stop = values
        .iter()
        .position(|v| v["message"] == "reached max_seconds (10s); stopping run")
        .unwrap();
    let ended = values
        .iter()
        .position(|v| v["type"] == "turn_ended" && v["stop_reason"] == "stop")
        .unwrap();
    assert!(stop < ended, "{values:?}");
    assert_eq!(
        values.last(),
        Some(&json!({"type": "run_ended", "turns": 2}))
    );
    assert_eq!(clock.load(Ordering::SeqCst), 10);
}

#[tokio::test]
async fn a_final_turn_that_asks_for_tools_runs_none_and_stops() {
    let (values, requests, agent, _) = run(
        vec![
            tool_call_response(&CALL),
            tool_call_response(&CALL),
            text_response("never requested"),
        ],
        10,
        0,
        Some(10),
    )
    .await;

    assert_eq!(
        warned(&values),
        vec![
            "reached max_seconds (10s); asking model for final response",
            "reached max_seconds (10s); stopping run",
        ],
        "{values:?}"
    );
    assert_eq!(requests.len(), 2, "no request follows the final turn");
    assert_eq!(
        values.last(),
        Some(&json!({"type": "run_ended", "turns": 2}))
    );
    // Every call is answered, so the session stays replay-safe, and the
    // answer says why the second call did not run.
    let results = tool_results(&agent);
    assert_eq!(results.len(), 2, "the final turn's call has a result too");
    assert_eq!(results[0], "t1");
    assert!(results[1].contains("time budget reached"), "{results:?}");
}

#[tokio::test]
async fn a_batch_that_outlives_the_budget_costs_no_final_request() {
    // The batch takes the clock past the budget. The batch itself ran,
    // since the budget was whole when it started, and the request after it
    // is the final one: the run still gets its answer.
    let (values, requests, agent, clock) = run(
        vec![tool_call_response(&CALL), text_response("done")],
        10,
        0,
        Some(20),
    )
    .await;

    assert_eq!(
        warned(&values),
        vec![
            "reached max_seconds (10s); asking model for final response",
            "reached max_seconds (10s); stopping run",
        ],
        "{values:?}"
    );
    assert_eq!(requests.len(), 2);
    assert_eq!(
        values.last(),
        Some(&json!({"type": "run_ended", "turns": 2}))
    );
    assert_eq!(tool_results(&agent), vec!["t1"]);
    assert_eq!(clock.load(Ordering::SeqCst), 20);
}

#[tokio::test]
async fn a_cut_on_the_final_turn_is_not_continued() {
    let (values, requests, _, _) = run(
        vec![tool_call_response(&CALL), length_response("partial")],
        10,
        0,
        Some(10),
    )
    .await;

    assert_eq!(
        warned(&values),
        vec![
            "reached max_seconds (10s); asking model for final response",
            "response truncated by max_tokens=512; reached max_seconds — the partial answer stands",
            "reached max_seconds (10s); stopping run",
        ],
        "{values:?}"
    );
    assert_eq!(requests.len(), 1 + 1, "the cut is not asked for again");
    assert_eq!(
        values.last(),
        Some(&json!({"type": "run_ended", "turns": 2}))
    );
}

#[tokio::test]
async fn the_clock_starts_with_the_run_and_needs_a_budget() {
    let (values, _, budgeted, _) = run(vec![text_response("done")], 1000, 900, None).await;
    assert_eq!(
        values.last(),
        Some(&json!({"type": "run_ended", "turns": 1}))
    );
    assert_eq!(budgeted.seconds_into_run(), Some(900));

    // `0` is no budget: the run has a clock and never reads it, however
    // far ahead of one it is.
    let (values, _, unlimited, _) = run(vec![text_response("done")], 0, 3600, None).await;
    assert_eq!(unlimited.max_seconds, None);
    assert!(warned(&values).is_empty(), "{values:?}");
    assert_eq!(unlimited.seconds_into_run(), Some(3600));

    // Before any run there is no clock at all.
    let bare = Agent::new(
        LlmClient::new("http://127.0.0.1:9").unwrap(),
        Registry::new(),
        "fake-model".into(),
        512,
        Arc::new(AtomicBool::new(false)),
        std::env::temp_dir(),
        None,
        false,
        131_072,
        Default::default(),
    );
    assert_eq!(bare.seconds_into_run(), None);
}
