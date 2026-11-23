//! Esc while a request is in flight (the in-flight half of GAP 3): the
//! streaming read watches the run's stop request, so a generation stops
//! when the user asks instead of at its own pace. The partial answer
//! survives, every tool call it started is answered, and the run ends with
//! the abort shape — never a truncation warning, never a continuation
//! request, never a `RunFailed`.

mod stall;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;

use super::super::{Registry, drive_full, event_values, messages_of};
use crate::agent::session::Message;
use crate::agent::session::Role;
use crate::agent::tools::{self, Tool};
use stall::StallingSse;

/// The abort, set from a task so it lands while the request is in flight.
fn abort_soon(abort: &Arc<AtomicBool>, after: Duration) -> tokio::task::JoinHandle<()> {
    let flag = abort.clone();
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        flag.store(true, Ordering::Relaxed);
    })
}

/// The same drive harness the other loop scripts use, with the abort flag
/// and the streaming path both explicit.
async fn drive_stopped(
    server: &StallingSse,
    registry: Registry,
    input: &str,
    abort: Arc<AtomicBool>,
) -> (
    anyhow::Result<()>,
    Vec<crate::agent::session::Entry>,
    Vec<crate::events::AgentEvent>,
) {
    drive_full(
        &server.base_url,
        registry,
        input,
        abort,
        None,
        true,
        Default::default(),
    )
    .await
}

/// Esc in the middle of a streamed answer: the run ends within moments,
/// the text the user already watched is recorded, and the abort shape is
/// the one every other stop path produces.
#[tokio::test]
async fn aborting_a_streaming_answer_keeps_it_and_ends_the_run() {
    let server = StallingSse::spawn(vec![StallingSse::half_text("half an answer")]);
    let abort = Arc::new(AtomicBool::new(false));
    let setter = abort_soon(&abort, Duration::from_millis(300));

    let started = Instant::now();
    let (result, entries, events) = tokio::time::timeout(
        Duration::from_secs(10),
        drive_stopped(&server, Registry::new(), "ask", abort),
    )
    .await
    .expect("the stop request must end the run, not the request timeout");
    setter.await.unwrap();

    assert!(
        result.is_ok(),
        "an abort is a clean stop, not a failure: {result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the run waited {:?} on a stream that had nothing left to send",
        started.elapsed()
    );

    let values = event_values(&events);
    // The deltas that arrived are on the record, and the run closes with
    // the abort shape: no turn marker, no truncation warning, no RunEnded.
    let deltas: Vec<&str> = values.iter().filter_map(|e| e["delta"].as_str()).collect();
    assert_eq!(
        deltas,
        vec!["half an", " answer"],
        "the deltas that arrived before the stop request are on the record"
    );
    assert_eq!(values.last().unwrap(), &json!({"type": "run_aborted"}));
    assert!(
        values
            .iter()
            .any(|e| e["type"] == "warned" && e["message"].as_str().unwrap() == "run aborted"),
        "{values:?}"
    );
    assert!(
        !values.iter().any(|e| e["type"] == "warned"
            && e["message"]
                .as_str()
                .unwrap()
                .contains("stream ended early")),
        "a stopped stream is not reported as a truncation: {values:?}"
    );
    assert!(
        !values.iter().any(|e| e["type"] == "turn_ended"),
        "an aborted run has no turn marker: {values:?}"
    );
    assert!(
        !values.iter().any(|e| e["type"] == "run_ended"),
        "an aborted run has no RunEnded: {values:?}"
    );

    // Session: the partial stands on its own entry, closed by the marker.
    let assistant: Vec<Option<&str>> = entries
        .iter()
        .map(messages_of)
        .filter(|m| m.role == Role::Assistant)
        .map(|m| m.content.as_deref())
        .collect();
    assert_eq!(assistant, vec![Some("half an answer")]);
    let last = messages_of(entries.last().unwrap());
    assert_eq!(last.role, Role::User);
    assert_eq!(last.content.as_deref(), Some("[aborted by user]"));

    // And no continuation: the user stopped the run, so no second request.
    assert_eq!(
        server.requests.lock().unwrap().len(),
        1,
        "an aborted stream is never continued"
    );
}

/// A stop request that lands before the response head: nothing assembled,
/// so nothing to keep — the run still ends with the abort shape rather than
/// as a failed run, and no blank assistant entry is recorded.
#[tokio::test]
async fn aborting_before_the_first_byte_is_an_abort_not_a_failure() {
    let server = StallingSse::spawn(vec![String::new()]);
    let abort = Arc::new(AtomicBool::new(false));
    let setter = abort_soon(&abort, Duration::from_millis(300));

    let (result, entries, events) = tokio::time::timeout(
        Duration::from_secs(10),
        drive_stopped(&server, Registry::new(), "ask", abort),
    )
    .await
    .expect("the stop request must end the run");
    setter.await.unwrap();

    assert!(result.is_ok(), "abort is not a run failure: {result:?}");
    let values = event_values(&events);
    assert_eq!(values.last().unwrap(), &json!({"type": "run_aborted"}));
    assert_eq!(
        values
            .iter()
            .filter(|e| e["type"] == "warned")
            .map(|e| e["message"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["run aborted"],
        "the stop request is the only thing this turn has to say"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|e| messages_of(e).role == Role::Assistant)
            .count(),
        0,
        "an empty partial earns no assistant entry"
    );
    let last = messages_of(entries.last().unwrap());
    assert_eq!(last.content.as_deref(), Some("[aborted by user]"));
    assert_eq!(
        server.requests.lock().unwrap().len(),
        1,
        "an aborted call is never retried"
    );
}

/// A stop request that lands mid-tool-call: the call is kept (the answer it
/// belongs to is already on the record) but never dispatched, and it is
/// answered with a synthetic error so the session stays replay-safe (§8).
#[tokio::test]
async fn aborting_mid_tool_call_answers_the_call_and_never_runs_it() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let server = StallingSse::spawn(vec![StallingSse::half_tool_call()]);
    let mut registry = Registry::new();
    registry.register(Box::new(Touch(marker.clone())));
    let abort = Arc::new(AtomicBool::new(false));
    let setter = abort_soon(&abort, Duration::from_millis(300));

    let (result, entries, events) = tokio::time::timeout(
        Duration::from_secs(10),
        drive_stopped(&server, registry, "echo hal", abort),
    )
    .await
    .expect("the stop request must end the run");
    setter.await.unwrap();

    assert!(result.is_ok(), "{result:?}");
    let values = event_values(&events);
    assert!(
        !values.iter().any(|e| e["type"] == "tool_call_started"),
        "a stopped call must never dispatch: {values:?}"
    );
    assert_eq!(values.last().unwrap(), &json!({"type": "run_aborted"}));

    let assistant = entries
        .iter()
        .map(messages_of)
        .find(|m| m.role == Role::Assistant)
        .expect("the partial call is kept");
    assert_eq!(assistant.tool_calls[0].id, "call_cut");

    let answers: Vec<&Message> = entries
        .iter()
        .map(messages_of)
        .filter(|m| m.role == Role::ToolResult)
        .collect();
    assert_eq!(answers.len(), 1, "the partial call is answered once");
    assert_eq!(answers[0].tool_call_id.as_deref(), Some("call_cut"));
    assert!(answers[0].is_error);
    assert_eq!(
        answers[0].content.as_deref(),
        Some("aborted before execution")
    );
    assert!(
        !marker.exists(),
        "a call the user stopped must not have run"
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1, "no continuation");
}

/// A tool whose only observable effect is a file: proof the stopped call
/// never ran. Its name matches the partial call's, so dispatch would have
/// found it.
struct Touch(std::path::PathBuf);

impl Tool for Touch {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn description(&self) -> &'static str {
        "create the marker file"
    }
    fn schema(&self) -> serde_json::Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}}})
    }
    fn execute<'a>(
        &'a self,
        _args: &'a serde_json::Value,
        _cancel: &'a tools::Cancel,
    ) -> tools::BoxFuture<'a, tools::ToolResult> {
        let path = self.0.clone();
        Box::pin(async move {
            let _ = std::fs::write(&path, "ran");
            tools::ToolResult::ok("touched".into())
        })
    }
}
