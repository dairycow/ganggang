//! Tests for the `task` tool against a fake [`SubagentRunner`]: the result
//! shape, the `Subtask` event flow to the parent, and the failure paths.

use std::sync::Arc;

use super::super::BoxFuture;
use super::*;
use crate::events::AgentEvent;

struct FakeRunner {
    text: String,
    turns: u32,
    session_id: Option<String>,
    err: Option<String>,
    emit_events: usize,
}

impl SubagentRunner for FakeRunner {
    fn run<'a>(
        &'a self,
        _req: TaskRequest,
        events: tokio::sync::mpsc::Sender<AgentEvent>,
    ) -> BoxFuture<'a, anyhow::Result<TaskOutcome>> {
        let text = self.text.clone();
        let turns = self.turns;
        let session_id = self.session_id.clone();
        let err = self.err.clone();
        let emit_events = self.emit_events;
        Box::pin(async move {
            for i in 0..emit_events {
                let _ = events
                    .send(AgentEvent::ToolCallStarted {
                        name: "read".into(),
                        id: format!("inner_{i}"),
                        args: serde_json::json!({}),
                    })
                    .await;
            }
            if let Some(err) = err {
                return Err(anyhow::anyhow!("{err}"));
            }
            Ok(TaskOutcome {
                text,
                turns,
                session_id,
                usage_in: 11,
                usage_out: 22,
            })
        })
    }
}

fn ctx_with_events(call_id: &str) -> (ToolCtx, tokio::sync::mpsc::Receiver<AgentEvent>) {
    ctx_with_events_capacity(call_id, 8)
}

fn ctx_with_events_capacity(
    call_id: &str,
    capacity: usize,
) -> (ToolCtx, tokio::sync::mpsc::Receiver<AgentEvent>) {
    let (tx, rx) = tokio::sync::mpsc::channel(capacity);
    let mut ctx = ToolCtx::none(&Cancel::none());
    ctx.call_id = call_id.to_string();
    ctx.events = Some(tx);
    (ctx, rx)
}

#[tokio::test]
async fn success_returns_the_answer_and_a_usage_summary() {
    let tool = TaskTool::new(Arc::new(FakeRunner {
        text: "the answer".into(),
        turns: 3,
        session_id: Some("s-1".into()),
        err: None,
        emit_events: 0,
    }));
    let res = tool
        .execute(&serde_json::json!({"prompt": "do it"}), &Cancel::none())
        .await;
    assert!(!res.is_error, "{}", res.output);
    assert_eq!(
        res.output,
        "the answer\n\n[subagent: 3 turns, 11 in / 22 out, session s-1]"
    );
}

#[tokio::test]
async fn success_omits_the_session_part_without_a_session() {
    let tool = TaskTool::new(Arc::new(FakeRunner {
        text: "ok".into(),
        turns: 1,
        session_id: None,
        err: None,
        emit_events: 0,
    }));
    let res = tool
        .execute(&serde_json::json!({"prompt": "do it"}), &Cancel::none())
        .await;
    assert!(!res.is_error, "{}", res.output);
    assert_eq!(res.output, "ok\n\n[subagent: 1 turns, 11 in / 22 out]");
}

/// The parent's event stream receives the child's events wrapped in
/// `Subtask`, tagged with the call's id, already buffered by the time the
/// tool returns: forwarding runs inline to completion, so a consumer that
/// stops at `RunEnded` never misses one.
#[tokio::test]
async fn parent_receives_subtask_events_tagged_with_the_call_id() {
    let tool = TaskTool::new(Arc::new(FakeRunner {
        text: "done".into(),
        turns: 2,
        session_id: None,
        err: None,
        emit_events: 1,
    }));
    let (ctx, mut rx) = ctx_with_events("call_7");
    let res = tool
        .execute_with(&serde_json::json!({"prompt": "do it"}), &ctx)
        .await;
    assert!(!res.is_error, "{}", res.output);
    let event = rx
        .try_recv()
        .expect("the nested event is buffered before the tool returns");
    match event {
        AgentEvent::Subtask { task_id, event } => {
            assert_eq!(task_id, "call_7");
            assert!(matches!(event.as_ref(), AgentEvent::ToolCallStarted { .. }));
        }
        other => panic!("expected Subtask, got {other:?}"),
    }
}

/// Several child events all land in the parent's buffer, in order, all
/// tagged with the call's id, before the tool returns. The parent channel
/// is sized for them so no event can be lost to backpressure.
#[tokio::test]
async fn every_child_event_is_buffered_in_order_before_the_tool_returns() {
    let tool = TaskTool::new(Arc::new(FakeRunner {
        text: "done".into(),
        turns: 2,
        session_id: None,
        err: None,
        emit_events: 5,
    }));
    let (ctx, mut rx) = ctx_with_events_capacity("call_7", 16);
    let res = tool
        .execute_with(&serde_json::json!({"prompt": "do it"}), &ctx)
        .await;
    assert!(!res.is_error, "{}", res.output);
    let mut ids = Vec::new();
    for i in 0..5 {
        let event = rx
            .try_recv()
            .unwrap_or_else(|_| panic!("event {i} is not buffered"));
        let AgentEvent::Subtask { task_id, event } = event else {
            panic!("expected Subtask, got {event:?}");
        };
        assert_eq!(task_id, "call_7");
        let AgentEvent::ToolCallStarted { id, .. } = event.as_ref() else {
            panic!("expected ToolCallStarted, got {event:?}");
        };
        ids.push(id.clone());
    }
    assert_eq!(
        ids,
        vec!["inner_0", "inner_1", "inner_2", "inner_3", "inner_4"]
    );
    assert!(rx.try_recv().is_err(), "no stray events");
}

/// A closed parent event receiver never blocks the tool: the forwarder
/// drops the late events and the run still finishes normally.
#[tokio::test]
async fn a_closed_parent_receiver_does_not_hang_the_tool() {
    let tool = TaskTool::new(Arc::new(FakeRunner {
        text: "done".into(),
        turns: 1,
        session_id: None,
        err: None,
        emit_events: 2,
    }));
    let (ctx, rx) = ctx_with_events_capacity("call_7", 16);
    drop(rx);
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tool.execute_with(&serde_json::json!({"prompt": "do it"}), &ctx),
    )
    .await
    .expect("the tool must finish with a closed parent receiver");
    assert!(!res.is_error, "{}", res.output);
    assert!(res.output.starts_with("done"));
}

#[tokio::test]
async fn a_runner_error_is_an_error_result() {
    let tool = TaskTool::new(Arc::new(FakeRunner {
        text: String::new(),
        turns: 0,
        session_id: None,
        err: Some("boom".into()),
        emit_events: 0,
    }));
    let res = tool
        .execute(&serde_json::json!({"prompt": "do it"}), &Cancel::none())
        .await;
    assert!(res.is_error);
    assert_eq!(res.output, "subagent failed: boom");
}

/// A stop request set by the time the result lands wins over a finished
/// child: the run already disowned the answer.
#[tokio::test]
async fn a_stopped_run_answers_aborted_by_user() {
    struct Stopper(Arc<std::sync::atomic::AtomicBool>);
    impl SubagentRunner for Stopper {
        fn run<'a>(
            &'a self,
            _req: TaskRequest,
            _events: tokio::sync::mpsc::Sender<AgentEvent>,
        ) -> BoxFuture<'a, anyhow::Result<TaskOutcome>> {
            let flag = self.0.clone();
            Box::pin(async move {
                flag.store(true, std::sync::atomic::Ordering::Release);
                Ok(TaskOutcome {
                    text: "too late".into(),
                    turns: 1,
                    session_id: None,
                    usage_in: 0,
                    usage_out: 0,
                })
            })
        }
    }
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let tool = TaskTool::new(Arc::new(Stopper(flag.clone())));
    let res = tool
        .execute(&serde_json::json!({"prompt": "do it"}), &Cancel::new(flag))
        .await;
    assert!(res.is_error);
    assert_eq!(res.output, "aborted by user");
}

#[tokio::test]
async fn a_missing_or_blank_prompt_is_an_argument_error() {
    let tool = TaskTool::new(Arc::new(FakeRunner {
        text: "x".into(),
        turns: 1,
        session_id: None,
        err: None,
        emit_events: 0,
    }));
    let res = tool.execute(&serde_json::json!({}), &Cancel::none()).await;
    assert!(res.is_error);
    assert!(res.output.contains("prompt"), "{}", res.output);

    let res = tool
        .execute(&serde_json::json!({"prompt": "   "}), &Cancel::none())
        .await;
    assert!(res.is_error);
    assert!(res.output.contains("prompt"), "{}", res.output);
}

/// A 20 KiB answer comes back capped at 10 KiB with the marker.
#[tokio::test]
async fn a_long_answer_is_truncated_at_10_kib() {
    let long = "x".repeat(20 * 1024);
    let tool = TaskTool::new(Arc::new(FakeRunner {
        text: long,
        turns: 1,
        session_id: None,
        err: None,
        emit_events: 0,
    }));
    let res = tool
        .execute(&serde_json::json!({"prompt": "go"}), &Cancel::none())
        .await;
    assert!(!res.is_error);
    assert!(
        res.output
            .ends_with("... [truncated]\n\n[subagent: 1 turns, 11 in / 22 out]")
    );
    let answer = res
        .output
        .strip_suffix("\n\n[subagent: 1 turns, 11 in / 22 out]")
        .unwrap();
    assert!(answer.ends_with("... [truncated]"));
    assert_eq!(
        answer.len(),
        10 * 1024,
        "the cut plus marker is exactly 10 KiB"
    );
}

/// A multibyte answer is cut on a char boundary within the same byte cap:
/// the old `chars().take(bytes)` would have overshot 10 KiB.
#[tokio::test]
async fn a_multibyte_answer_is_truncated_on_a_char_boundary() {
    let long = "é".repeat(6000);
    let tool = TaskTool::new(Arc::new(FakeRunner {
        text: long,
        turns: 1,
        session_id: None,
        err: None,
        emit_events: 0,
    }));
    let res = tool
        .execute(&serde_json::json!({"prompt": "go"}), &Cancel::none())
        .await;
    assert!(!res.is_error);
    let answer = res
        .output
        .strip_suffix("\n\n[subagent: 1 turns, 11 in / 22 out]")
        .unwrap();
    assert!(answer.ends_with("... [truncated]"));
    assert!(answer.len() <= 10 * 1024, "{} bytes", answer.len());
    assert!(
        answer.len() > 10 * 1024 - 16,
        "the cut fills the byte budget: {} bytes",
        answer.len()
    );
}

/// The schema and the advertisement shape: the model sees one required
/// parameter and three optional ones.
#[test]
fn the_schema_names_the_parameters_and_requires_prompt() {
    let tool = TaskTool::new(Arc::new(FakeRunner {
        text: "x".into(),
        turns: 1,
        session_id: None,
        err: None,
        emit_events: 0,
    }));
    assert_eq!(tool.name(), "task");
    assert!(!tool.is_read_only());
    let schema = tool.schema();
    let props = schema["properties"].as_object().unwrap();
    assert!(props.contains_key("prompt"));
    assert!(props.contains_key("model"));
    assert!(props.contains_key("tools"));
    assert!(props.contains_key("max_turns"));
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(required, ["prompt"]);
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn narrow_tools_defaults_to_the_parent_allowlist() {
    let parent = strings(&["read", "bash"]);
    assert_eq!(narrow_tools(None, &parent).unwrap(), parent);
}

#[test]
fn narrow_tools_accepts_a_subset_of_the_parent_and_deduplicates() {
    let parent = strings(&["read", "bash", "write"]);
    assert_eq!(
        narrow_tools(Some(strings(&["write", "read"])), &parent).unwrap(),
        strings(&["write", "read"])
    );
    assert_eq!(
        narrow_tools(Some(strings(&["read", "read"])), &parent).unwrap(),
        strings(&["read"])
    );
}

#[test]
fn narrow_tools_rejects_a_name_outside_the_parent() {
    let parent = strings(&["read"]);
    let err = narrow_tools(Some(strings(&["bash"])), &parent).unwrap_err();
    assert!(err.to_string().contains("bash"), "{err}");
}

#[test]
fn narrow_tools_rejects_task_to_keep_the_one_level_rule() {
    let parent = strings(&["read"]);
    let err = narrow_tools(Some(strings(&["task"])), &parent).unwrap_err();
    assert!(err.to_string().contains("one level deep"), "{err}");
}

#[test]
fn effective_max_turns_caps_the_request_at_the_run_budget() {
    assert_eq!(effective_max_turns(Some(5), Some(10)).unwrap(), Some(5));
    assert_eq!(effective_max_turns(Some(50), Some(10)).unwrap(), Some(10));
}

#[test]
fn effective_max_turns_falls_back_to_the_run_budget_when_unrequested() {
    assert_eq!(effective_max_turns(None, Some(10)).unwrap(), Some(10));
    assert_eq!(effective_max_turns(Some(5), None).unwrap(), Some(5));
    assert_eq!(effective_max_turns(None, None).unwrap(), None);
}

#[test]
fn effective_max_turns_rejects_zero() {
    let err = effective_max_turns(Some(0), Some(10)).unwrap_err();
    assert!(err.to_string().contains("max_turns"), "{err}");
}

/// `max_turns: 0` is rejected as an invalid-arguments result before the
/// subagent starts: a zero budget must not silently lift the run's cap.
#[tokio::test]
async fn a_zero_max_turns_is_an_argument_error_before_the_subagent_starts() {
    let tool = TaskTool::new(Arc::new(FakeRunner {
        text: "should not run".into(),
        turns: 1,
        session_id: None,
        err: None,
        emit_events: 0,
    }));
    let res = tool
        .execute(
            &serde_json::json!({"prompt": "do it", "max_turns": 0}),
            &Cancel::none(),
        )
        .await;
    assert!(res.is_error);
    assert_eq!(
        res.output,
        "invalid arguments: max_turns must be at least 1"
    );
}
